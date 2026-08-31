#!/usr/bin/env python3
"""Les chiffres du conteneur, en fichier plutot qu'en sortie de terminal.

La carte precedente a rendu la mesure honnete : un seul chiffre publie, sa
definition ecrite a cote, la meme des deux cotes, et deux defauts de protocole
corriges. Son resultat n'existait pourtant que dans une sortie de terminal,
recopiee a la main dans le README — donc invisible a un lecteur, et derivant des
qu'un octet bouge. C'est le probleme que `docs/conformance.json` a resolu pour
la conformance et `docs/compat.json` pour le perimetre.

Trois modes, et le troisieme est celui qui tient les deux autres :

    ./tests/compat/measure_container.sh --json docs/container.json   # mesurer
    python3 tests/compat/chiffres_conteneur.py --injecte             # publier
    python3 tests/compat/chiffres_conteneur.py --verifie             # le cliquet

`--injecte` reecrit, dans le README et dans `docs/bench.md`, les blocs bornes
par `<!-- chiffres-conteneur:… -->` **depuis** `docs/container.json`, et les
quelques phrases qui citent un de ces nombres au fil du texte. `--verifie` fait
le meme calcul et echoue si un caractere differe : c'est ce que lance la CI,
comme `genere_compat.py --verifie`. Un chiffre de conteneur saisi a la main sans
passer par la ne peut donc plus exister — et un motif introuvable est une
**erreur**, pas un silence : une verification qui ne trouve rien a comparer ne
doit pas rendre de verdict vert.

Ce que la campagne mesure, et pourquoi les deux images sont dans le meme
fichier : la seule exigence d'une comparaison est que ses deux colonnes portent
la meme definition, ce qui ne se prouve qu'en les mesurant avec le meme outil,
dans la meme campagne, sur la meme machine.
"""

from __future__ import annotations

import argparse
import io
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from contextlib import redirect_stdout
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import tailles_image  # noqa: E402

RACINE = Path(__file__).resolve().parents[2]
RAPPORT = RACINE / "docs" / "container.json"
SCHEMA = 1

# Les deux images que le README compare, et les arguments exacts de leur
# `docker run`. Ils font partie de la definition du RSS et du demarrage : un ES
# lance avec une autre heap ne se compare pas a celui d'hier.
IMAGES = [
    {
        "nom": "ferrite",
        # `None` : la reference se lit dans Cargo.toml a la campagne, voir
        # `reference_ferrite`. Elle a ete ecrite en dur ici, et la release 0.8.0
        # l'a paye.
        "reference": None,
        "origine": "construite ici depuis le Dockerfile du depot (`scratch` + binaire statique musl)",
        "run": [],
    },
    {
        "nom": "elasticsearch",
        "reference": "docker.elastic.co/elasticsearch/elasticsearch:8.15.0",
        "origine": "image officielle d'Elastic, tiree telle quelle",
        "run": [
            "-e",
            "discovery.type=single-node",
            "-e",
            "xpack.security.enabled=false",
            "-e",
            "ES_JAVA_OPTS=-Xms512m -Xmx512m",
        ],
    },
]

# Un nombre sans sa definition est un nombre faux en puissance : ce depot a
# publie trois valeurs differentes pour la meme image, toutes justes sous une
# definition que personne n'avait ecrite. Elles sont donc dans le rapport, a
# cote des nombres.
DEFINITIONS = {
    "compressee_octets": (
        "La somme des blobs qu'un registre sert pour cette image — le manifeste, "
        "la configuration et les couches telles qu'elles sont poussees. C'est ce "
        "qu'un `docker pull` telecharge, et le seul chiffre que ce depot publie. "
        "`null` quand l'archive porte des couches nues : la taille qu'un registre "
        "servirait n'en est alors pas deductible, et un nombre plausible mis a sa "
        "place serait la taille decompressee sous le nom de l'autre."
    ),
    "decompressee_octets": (
        "La somme des couches une fois depliees, c'est-a-dire ce que le systeme "
        "de fichiers de l'image occupe une fois l'image tiree."
    ),
    "binaire_octets": (
        "La taille du fichier que l'ENTRYPOINT du conteneur execute. `null` quand "
        "l'ENTRYPOINT n'est pas un binaire — celui d'Elasticsearch est un lien "
        "vers un script qui lance une JVM, et un nombre qui ne se compare a rien "
        "vaut moins que son absence."
    ),
    "rss_repos_ko": (
        "La somme des VmRSS de **tous** les processus du cgroup du conteneur, en "
        "kilo-octets, relevee 2 s apres le premier `GET /` servi et avant qu'aucun "
        "document ne soit indexe. Lire le seul PID 1 rendrait le RSS du shell qui "
        "lance la JVM d'Elasticsearch : deux ordres de grandeur en dessous, et du "
        "bon cote pour nous."
    ),
    "demarrage_ms": (
        "Le temps entre `docker run -d` et le premier `GET /` servi en 200, "
        "mediane de `mesure.tours_demarrage` demarrages successifs (conteneur "
        "recree a chaque tour). Pour ferrite l'essentiel est la creation du "
        "conteneur par Docker."
    ),
    "digest_manifeste": (
        "Le digest du manifeste de la plateforme locale dans l'archive mesuree : "
        "il dit exactement quels octets ont ete comptes."
    ),
    "couches": "Le nombre de couches du manifeste de cette plateforme.",
    "couches_nues": (
        "Les couches de l'archive dont les octets ne sont pas compresses. Une "
        "seule suffit a rendre `compressee_octets` non deductible : `docker save` "
        "n'ecrit des blobs compresses que depuis le magasin d'images containerd."
    ),
    "demarrage_tours_ms": (
        "Les `mesure.tours_demarrage` demarrages dont `demarrage_ms` est la "
        "mediane, dans l'ordre. Publies parce qu'une mediane seule ne dit pas si "
        "les tours sont serres."
    ),
    "docker_run": (
        "Les arguments exacts du `docker run` de la mesure. Ils font partie de la "
        "definition du RSS et du demarrage : un Elasticsearch lance avec une autre "
        "heap ne se compare pas a celui d'hier."
    ),
    "origine": "D'ou vient l'image mesuree : construite ici, ou tiree telle quelle.",
    "build_hash": (
        "Le `version.build_hash` que le serveur annonce sur `GET /` pendant la "
        "campagne — ce que le binaire dit de lui-meme, et la seule chose ici qui "
        "ne soit pas un nom qu'on a tape. Pour ferrite c'est "
        "`ferrite-{version de Cargo.toml}` : un tag dit ce qu'on a voulu "
        "construire, ce champ dit ce qui a repondu."
    ),
    "mesure.ferrite_arbre_modifie": (
        "Vrai si un des fichiers dont l'image est faite (`src/`, `Cargo.toml`, "
        "`Cargo.lock`, `Dockerfile`) differait de `mesure.ferrite_sha` pendant la "
        "campagne — donc si l'image mesuree pourrait ne pas etre celle que ce "
        "commit produit. Un README modifie au meme moment ne compte pas : il ne "
        "change pas un octet du binaire."
    ),
}

POURQUOI_DOCKER = (
    "La version du serveur Docker et son magasin d'images ne changent pas ce que "
    "cet outil mesure — il compte les octets de l'archive — mais ils changent ce "
    "que les AUTRES outils repondent a la meme question : "
    "`docker image inspect --format '{{.Size}}'` rendait la taille decompressee "
    "jusqu'a Docker 28 et rend la compressee depuis la 29, et la colonne "
    "DISK USAGE de `docker images` en additionne encore une troisieme."
)


# --- petites mises en forme, cote francais ----------------------------------


def nombre(x: float, decimales: int = 1) -> str:
    return f"{x:,.{decimales}f}".replace(",", " ").replace(".", ",")


def taille(octets: int) -> str:
    """Megaoctets decimaux (10^6), les memes que ceux de `docker images`."""
    return f"{nombre(octets / 1_000_000)} Mo"


def mio(octets: int) -> str:
    return nombre(octets / 2**20)


def memoire(ko: int) -> str:
    mo_ = ko * 1024 / 1_000_000
    return f"{nombre(mo_ / 1000, 2)} Go" if mo_ >= 1000 else f"{nombre(mo_)} Mo"


def duree(ms: int) -> str:
    return f"{nombre(ms / 1000)} s" if ms >= 1000 else f"{ms} ms"


def rapport(grand: float, petit: float) -> str:
    return f"×{round(grand / petit)}"


# --- la mesure --------------------------------------------------------------


def docker(*args: str) -> str:
    return subprocess.run(
        ["docker", *args], capture_output=True, text=True, check=True
    ).stdout.strip()


def hote() -> dict:
    return {
        "docker_serveur": docker("version", "--format", "{{.Server.Version}}"),
        "magasin_images": docker("info", "--format", "{{.Driver}}"),
        "plateforme": docker("version", "--format", "{{.Server.Os}}/{{.Server.Arch}}"),
    }


def tailles(reference: str, archive: str | None = None) -> dict:
    """Les tailles d'une image, comptees sur ses octets.

    `docker save` ne rend les blobs compresses que depuis le magasin d'images
    containerd ; $IMAGE_TAR laisse passer directement l'artefact OCI de buildx —
    ce qu'un `docker push` enverrait, et ce que fait la CI.
    """
    if archive:
        return tailles_image.mesure(archive, reference)
    tmp = tempfile.mkdtemp()
    try:
        chemin = os.path.join(tmp, "image.tar")
        docker("save", reference, "-o", chemin)
        return tailles_image.mesure(chemin, reference)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def vivant(nom: str) -> bool:
    return docker("inspect", "-f", "{{.State.Running}}", nom) == "true"


def attend(port: int, nom: str, limite_s: float = 300.0) -> None:
    """Attend le premier `GET /` servi. Un conteneur mort est une erreur.

    Le `docker inspect` qui verifie que le conteneur vit coute quelques dizaines
    de millisecondes : le poser a chaque tour de boucle mesurerait la sonde
    plutot que le serveur, et ferrite demarre en moins de 200 ms. Il est donc
    espace d'une seconde — ce que ce depot a deja paye deux fois sur le banc a
    l'echelle, ou une constante commune aux deux serveurs ecrasait l'ecart.
    """
    debut = time.monotonic()
    dernier_controle = debut
    while True:
        try:
            with urllib.request.urlopen(
                f"http://127.0.0.1:{port}/", timeout=2
            ) as reponse:
                if reponse.status == 200:
                    return
        except (urllib.error.URLError, OSError):
            pass
        maintenant = time.monotonic()
        if maintenant - dernier_controle >= 1.0:
            dernier_controle = maintenant
            if not vivant(nom):
                journal = subprocess.run(
                    ["docker", "logs", "--tail", "20", nom],
                    capture_output=True,
                    text=True,
                )
                raise SystemExit(
                    f"{nom} s'est arrete avant de servir un GET / :\n"
                    f"{journal.stdout}{journal.stderr}"
                )
        if maintenant - debut > limite_s:
            raise SystemExit(f"{nom} n'a pas servi de GET / en {limite_s:.0f} s")
        time.sleep(0.005)


def build_hash(port: int) -> str | None:
    """Ce que le serveur qui vient de repondre dit de lui-meme.

    Le tag d'une image dit ce qu'on a voulu construire ; ce champ dit quel
    binaire a servi le `GET /` qu'on vient de chronometrer. C'est la seule
    piece de la campagne que personne ne tape.
    """
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/", timeout=5) as reponse:
            return json.load(reponse).get("version", {}).get("build_hash")
    except (urllib.error.URLError, OSError, ValueError):
        return None


def rss_du_cgroup(cid: str) -> int:
    """La somme des VmRSS de tous les processus du cgroup du conteneur, en Ko."""
    total = 0
    for proc in Path("/proc").iterdir():
        if not proc.name.isdigit():
            continue
        try:
            if cid not in (proc / "cgroup").read_text():
                continue
            for ligne in (proc / "status").read_text().splitlines():
                if ligne.startswith("VmRSS:"):
                    total += int(ligne.split()[1])
        except (OSError, ValueError):
            continue  # le processus a disparu entre deux lectures
    if total == 0:
        # Zero se lirait « ce serveur ne consomme rien », soit le plus flatteur
        # des echecs silencieux.
        raise SystemExit(
            f"RSS : aucun processus trouve dans le cgroup {cid[:12]} — "
            "la mesure est impossible, elle n'est pas nulle"
        )
    return total


def demarrage_et_rss(reference: str, run: list[str], port: int, tours: int) -> dict:
    """Demarre le conteneur `tours` fois, et releve le RSS sur le dernier."""
    temps = []
    rss = None
    annonce = None
    for tour in range(tours):
        nom = f"mesure-conteneur-{os.getpid()}-{tour}"
        subprocess.run(["docker", "rm", "-f", nom], capture_output=True)
        debut = time.monotonic_ns()
        subprocess.run(
            ["docker", "run", "-d", "--name", nom, "-p", f"{port}:9200", *run, reference],
            check=True,
            capture_output=True,
        )
        try:
            attend(port, nom)
            temps.append((time.monotonic_ns() - debut) // 1_000_000)
            if tour == tours - 1:
                cid = docker("inspect", "-f", "{{.Id}}", nom)
                annonce = build_hash(port)
                time.sleep(2)  # laisser le serveur se poser
                rss = rss_du_cgroup(cid)
        finally:
            subprocess.run(["docker", "rm", "-f", nom], capture_output=True)
    return {
        "demarrage_ms": int(statistics.median(temps)),
        "demarrage_tours_ms": temps,
        "rss_repos_ko": rss,
        "build_hash": annonce,
    }


# Ce qui fabrique l'image. Un README modifie pendant la campagne ne change pas
# un octet du binaire ; un `src/` modifie, si — et c'est cette question-la que
# le rapport doit poser.
CHEMINS_IMAGE = ["src", "Cargo.toml", "Cargo.lock", "Dockerfile"]


def sha_ferrite() -> tuple[str, bool]:
    sha = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
        cwd=RACINE,
    ).stdout.strip()
    modifie = bool(
        subprocess.run(
            ["git", "status", "--porcelain", "--", *CHEMINS_IMAGE],
            capture_output=True,
            text=True,
            cwd=RACINE,
        ).stdout.strip()
    )
    return sha, modifie


def version_ferrite() -> str:
    for ligne in (RACINE / "Cargo.toml").read_text(encoding="utf-8").splitlines():
        if ligne.startswith("version = "):
            return ligne.split('"')[1]
    raise SystemExit("version absente de Cargo.toml")


def reference_ferrite() -> str:
    """`ferrite:{la version de Cargo.toml}` — elle ne se retape nulle part.

    Elle etait ecrite en dur dans ce fichier, et la release 0.8.0 l'a paye : le
    bump de version laissait la campagne mesurer l'image `ferrite:0.7.0` — le
    binaire d'avant — tout en ecrivant `ferrite_version: 0.8.0` dans le rapport,
    puis `ferrite 0.8.0` dans le tableau du README. Soit la taille d'un binaire
    publiee sous le numero d'un autre, c'est-a-dire exactement ce que le
    controle de `charge()` empeche du cote de la publication. Il manquait du
    cote de la mesure.
    """
    return f"ferrite:{version_ferrite()}"


def image_presente(reference: str) -> bool:
    return (
        subprocess.run(
            ["docker", "image", "inspect", reference], capture_output=True
        ).returncode
        == 0
    )


def rend_humain(machine: dict, image: dict) -> str:
    """Ce que l'outil imprime pour une image — la source du bloc du README.

    Le bloc publie est cette sortie-la, pas sa transcription : une sortie de
    terminal recopiee a la main est exactement ce que cette carte supprime.
    """
    lignes = [
        f"docker serveur   : {machine['docker_serveur']}"
        f" (magasin d'images : {machine['magasin_images']})",
        "",
    ]
    tampon = io.StringIO()
    with redirect_stdout(tampon):
        tailles_image.imprime(image)
    lignes.append(tampon.getvalue().rstrip("\n"))
    if image.get("demarrage_ms") is not None:
        lignes.append("")
        lignes.append(
            f"demarrage        : {image['demarrage_ms']} ms"
            f" (docker run -> premier GET / servi, mediane de"
            f" {len(image['demarrage_tours_ms'])} tours)"
        )
        lignes.append(
            f"RSS au repos     : {tailles_image.espace(image['rss_repos_ko'])} Ko"
            f" ({tailles_image.mo(image['rss_repos_ko'] * 1024)})"
        )
    return "\n".join(lignes)


def campagne(tours: int, port: int, archive_ferrite: str | None) -> dict:
    machine = hote()
    sha, modifie = sha_ferrite()
    images = []
    version = version_ferrite()
    for spec in IMAGES:
        reference = spec["reference"] or reference_ferrite()
        archive = archive_ferrite if spec["nom"] == "ferrite" else None
        print(f"== {spec['nom']} : {reference}", file=sys.stderr)
        if not archive and not image_presente(reference):
            # Une image absente doit se dire, pas se lire dans la trace d'un
            # `docker save` qui echoue.
            quoi = (
                f"docker build -t {reference} ."
                if spec["nom"] == "ferrite"
                else f"docker pull {reference}"
            )
            raise SystemExit(
                f"l'image {reference} n'existe pas sur cette machine — "
                f"la campagne mesure ce qui est declare, pas ce qui traine :\n"
                f"    {quoi}"
            )
        # L'identite d'abord, les nombres ensuite : une mesure qu'on ne peut pas
        # rattacher a des octets precis ne se relit pas.
        image = {
            "nom": spec["nom"],
            "reference": reference,
            "tag": reference.rsplit(":", 1)[-1],
            "origine": spec["origine"],
            "docker_run": ["-p", f"{port}:9200", *spec["run"]],
        }
        image.update(tailles(reference, archive))
        image.update(demarrage_et_rss(reference, spec["run"], port, tours))
        if spec["nom"] == "ferrite" and image["build_hash"] != f"ferrite-{version}":
            # Le tag dit ce qu'on a voulu construire ; c'est le binaire qui a
            # repondu qui dit ce qu'on a mesure. Une image reconstruite sans
            # avoir recompile porterait le bon nom et les mauvais octets.
            raise SystemExit(
                f"{reference} annonce build_hash {image['build_hash']!r}, "
                f"Cargo.toml declare {version} — l'image n'a pas ete "
                f"reconstruite depuis le bump :\n"
                f"    docker build -t {reference} ."
            )
        images.append(image)
        print(rend_humain(machine, image))
        print()
    return {
        "schema": SCHEMA,
        "mesure": {
            "date": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "ferrite_version": version_ferrite(),
            "ferrite_sha": sha,
            "ferrite_arbre_modifie": modifie,
            "outil": "./tests/compat/measure_container.sh --json docs/container.json",
            "tours_demarrage": tours,
            "hote": machine,
            "pourquoi_la_version_de_docker_est_ici": POURQUOI_DOCKER,
        },
        "definitions": DEFINITIONS,
        "images": images,
    }


# --- ce que le rapport publie -----------------------------------------------


def image(donnees: dict, nom: str) -> dict:
    for i in donnees["images"]:
        if i["nom"] == nom:
            return i
    raise SystemExit(f"{RAPPORT} ne porte pas de mesure pour {nom}")


def blocs(donnees: dict) -> dict[tuple[str, str], str]:
    """Les blocs generes, ranges par (fichier, nom du marqueur)."""
    f, e = image(donnees, "ferrite"), image(donnees, "elasticsearch")
    tours = donnees["mesure"]["tours_demarrage"]
    es = f"Elasticsearch {e['tag']}"

    apercu = [
        f"|  | {es} | ferrite |",
        "|---|---|---|",
        f"| Image compressée, telle qu'un registre la sert |"
        f" {taille(e['compressee_octets'])} |"
        f" **{taille(f['compressee_octets'])}** (`scratch`) |",
        f"| RSS au repos | {memoire(e['rss_repos_ko'])} |"
        f" **{memoire(f['rss_repos_ko'])}** |",
        f"| Démarrage | {duree(e['demarrage_ms'])} |"
        f" **{duree(f['demarrage_ms'])}** (`docker run` → premier `GET /` servi) |",
        "| Runtime | JVM + tuning heap | un binaire statique |",
    ]

    tableau = [
        f"| | {es} | ferrite {f['tag']} | × |",
        "|---|---|---|---|",
        f"| **Image compressée**, telle qu'un registre la sert |"
        f" {taille(e['compressee_octets'])} |"
        f" **{taille(f['compressee_octets'])}** |"
        f" **{rapport(e['compressee_octets'], f['compressee_octets'])}** |",
        f"| Image décompressée, ce que son système de fichiers occupe |"
        f" {taille(e['decompressee_octets'])} |"
        f" {taille(f['decompressee_octets'])} |"
        f" {rapport(e['decompressee_octets'], f['decompressee_octets'])} |",
        f"| Le binaire seul | — | {taille(f['binaire_octets'])} | |",
        f"| Mémoire au repos (RSS) | {memoire(e['rss_repos_ko'])} |"
        f" **{memoire(f['rss_repos_ko'])}** |"
        f" **{rapport(e['rss_repos_ko'], f['rss_repos_ko'])}** |",
        f"| Démarrage (`docker run` → premier `GET /` servi) |"
        f" {duree(e['demarrage_ms'])} |"
        f" **{duree(f['demarrage_ms'])}** (médiane de {tours} ; l'essentiel est la"
        f" création du conteneur par Docker) |"
        f" {rapport(e['demarrage_ms'], f['demarrage_ms'])} |",
    ]

    unites = [
        "Les « Mo » sont des mégaoctets décimaux (10⁶ octets), les mêmes que ceux",
        "qu'affiche `docker images` ; en Mio (2²⁰) la première ligne se lirait",
        f"{mio(e['compressee_octets'])} contre {mio(f['compressee_octets'])}.",
    ]

    sortie = [
        "```",
        f"$ ./tests/compat/measure_container.sh {f['reference']}",
        rend_humain(donnees["mesure"]["hote"], f),
        "```",
    ]

    enveloppe = [
        f"| | ferrite {f['tag']} | ES {e['tag']} | × |",
        "|---|---|---|---|",
        f"| **Image compressée**, telle qu'un registre la sert |"
        f" **{taille(f['compressee_octets'])}** |"
        f" {taille(e['compressee_octets'])} |"
        f" **{rapport(e['compressee_octets'], f['compressee_octets'])}** |",
        f"| Image décompressée, ce que son système de fichiers occupe |"
        f" {taille(f['decompressee_octets'])} |"
        f" {taille(e['decompressee_octets'])} |"
        f" {rapport(e['decompressee_octets'], f['decompressee_octets'])} |",
        f"| Le binaire seul | {taille(f['binaire_octets'])} | — (une JVM) | |",
    ]

    return {
        ("README.md", "apercu"): "\n".join(apercu),
        ("README.md", "tableau"): "\n".join(tableau),
        ("README.md", "unites"): "\n".join(unites),
        ("README.md", "sortie"): "\n".join(sortie),
        ("docs/bench.md", "enveloppe"): "\n".join(enveloppe),
    }


def phrases(donnees: dict) -> list[tuple[str, str, str]]:
    """Les nombres cites au fil du texte : (fichier, motif, valeur attendue).

    Un bloc genere ne peut pas s'ouvrir au milieu d'un paragraphe sans le
    couper ; ces trois-la sont donc verifies par leur motif, et le motif
    introuvable est une erreur.
    """
    f, e = image(donnees, "ferrite"), image(donnees, "elasticsearch")
    return [
        (
            "CLAUDE.md",
            r"image de ([\d , ]+ Mo) compressés",
            taille(f["compressee_octets"]),
        ),
        (
            "docs/bench.md",
            r"\*\*([\d , ]+ Mo) compressés\*\* au sens de",
            taille(f["compressee_octets"]),
        ),
        (
            "docs/bench.md",
            r"contre ([\d , ]+ Mo) pour celle d'Elasticsearch",
            taille(e["compressee_octets"]),
        ),
    ]


def marqueurs(nom: str) -> tuple[str, str]:
    return (
        f"<!-- chiffres-conteneur:{nom} — généré depuis docs/container.json par"
        f" `python3 tests/compat/chiffres_conteneur.py --injecte`, ne pas éditer"
        f" à la main -->",
        f"<!-- /chiffres-conteneur:{nom} -->",
    )


def applique(donnees: dict, ecrire: bool) -> list[str]:
    """Injecte (ou verifie) les blocs et les phrases. Rend les ecarts."""
    ecarts: list[str] = []
    textes: dict[str, str] = {}

    def lis(fichier: str) -> str:
        if fichier not in textes:
            textes[fichier] = (RACINE / fichier).read_text(encoding="utf-8")
        return textes[fichier]

    for (fichier, nom), attendu in blocs(donnees).items():
        texte = lis(fichier)
        ouvre, ferme = marqueurs(nom)
        motif = re.compile(
            rf"(?s)(<!-- chiffres-conteneur:{nom} [^>]*-->\n)(.*?)(\n<!-- /chiffres-conteneur:{nom} -->)"
        )
        trouve = motif.search(texte)
        if not trouve:
            # Un marqueur absent n'est pas « rien a faire » : c'est un bloc
            # publie qui a cesse d'etre tenu.
            ecarts.append(f"{fichier} : marqueur chiffres-conteneur:{nom} introuvable")
            continue
        if trouve.group(2) == attendu:
            continue
        ecarts.append(
            f"{fichier} : le bloc « {nom} » ne correspond plus à docs/container.json"
        )
        textes[fichier] = (
            texte[: trouve.start()]
            + ouvre
            + "\n"
            + attendu
            + "\n"
            + ferme
            + texte[trouve.end() :]
        )

    for fichier, motif, attendu in phrases(donnees):
        texte = lis(fichier)
        trouve = re.search(motif, texte)
        if not trouve:
            ecarts.append(f"{fichier} : phrase introuvable (motif {motif!r})")
            continue
        if trouve.group(1) == attendu:
            continue
        ecarts.append(
            f"{fichier} : « {trouve.group(1)} » cité au fil du texte, "
            f"« {attendu} » mesuré"
        )
        textes[fichier] = (
            texte[: trouve.start(1)] + attendu + texte[trouve.end(1) :]
        )

    if ecrire:
        for fichier, texte in textes.items():
            (RACINE / fichier).write_text(texte, encoding="utf-8")
    return ecarts


def charge() -> dict:
    if not RAPPORT.exists():
        raise SystemExit(
            f"{RAPPORT} absent — la campagne l'ecrit :\n"
            "    ./tests/compat/measure_container.sh --json docs/container.json"
        )
    donnees = json.loads(RAPPORT.read_text(encoding="utf-8"))
    if donnees.get("schema") != SCHEMA:
        raise SystemExit(
            f"{RAPPORT} est au schema {donnees.get('schema')}, cet outil lit le {SCHEMA}"
        )
    attendue = version_ferrite()
    if donnees["mesure"]["ferrite_version"] != attendue:
        # Le binaire a change de version, donc de taille : republier l'ancienne
        # mesure sous le nouveau numero serait le defaut que ce fichier repare.
        raise SystemExit(
            f"{RAPPORT} mesure ferrite {donnees['mesure']['ferrite_version']}, "
            f"Cargo.toml declare {attendue} — la campagne est a relancer :\n"
            f"    docker build -t ferrite:{attendue} . && "
            "./tests/compat/measure_container.sh --json docs/container.json"
        )
    # `mesure.ferrite_version` est lu dans Cargo.toml au moment de la campagne :
    # il dit ce qui etait declare, pas ce qui a ete mesure. Les deux champs qui
    # le disent sont le tag de l'image et le `build_hash` que le binaire a
    # annonce sur `GET /` — sans ce controle, une campagne lancee sur l'image
    # d'avant republierait ses octets sous le numero du jour, et le controle
    # ci-dessus serait vert.
    ferrite = image(donnees, "ferrite")
    mesures = {"tag": ferrite.get("tag"), "build_hash": ferrite.get("build_hash")}
    voulues = {"tag": attendue, "build_hash": f"ferrite-{attendue}"}
    if mesures != voulues:
        raise SystemExit(
            f"{RAPPORT} dit ferrite {attendue} mais a mesure l'image "
            f"{ferrite.get('reference')!r} "
            f"(tag {mesures['tag']!r}, build_hash {mesures['build_hash']!r}) — "
            f"la campagne est a relancer :\n"
            f"    docker build -t ferrite:{attendue} . && "
            "./tests/compat/measure_container.sh --json docs/container.json"
        )
    return donnees


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--mesure", action="store_true", help="lance la campagne")
    p.add_argument("--json", metavar="FICHIER", nargs="?", const=str(RAPPORT))
    p.add_argument("--tours", type=int, default=5)
    p.add_argument("--port", type=int, default=int(os.environ.get("PORT", 9299)))
    p.add_argument("--tailles", nargs="+", metavar="IMAGE")
    p.add_argument("--image", metavar="REF")
    p.add_argument("--run", nargs=argparse.REMAINDER, default=[])
    p.add_argument("--injecte", action="store_true")
    p.add_argument("--verifie", action="store_true")
    args = p.parse_args()
    archive = os.environ.get("IMAGE_TAR") or None

    if args.injecte or args.verifie:
        donnees = charge()
        ecarts = applique(donnees, ecrire=args.injecte)
        if not ecarts:
            print("chiffres du conteneur : documentation conforme à docs/container.json")
            return 0
        for e in ecarts:
            print(("réécrit  " if args.injecte else "ÉCART    ") + e)
        if args.injecte:
            return 0
        print(
            "\nCes chiffres sont publiés : ils se régénèrent, ils ne se retapent pas.\n"
            "    python3 tests/compat/chiffres_conteneur.py --injecte",
            file=sys.stderr,
        )
        return 1

    if args.tailles:
        machine = hote()
        print(
            f"docker serveur   : {machine['docker_serveur']}"
            f" (magasin d'images : {machine['magasin_images']})"
        )
        code = 0
        for reference in args.tailles:
            print()
            code |= tailles_image.imprime(tailles(reference, archive))
        return code

    if args.image:
        machine = hote()
        m = tailles(args.image, archive)
        m.update(demarrage_et_rss(args.image, args.run, args.port, args.tours))
        print(rend_humain(machine, m))
        return 0 if m["compressee_octets"] is not None else 1

    if not args.mesure:
        p.print_help()
        return 2

    donnees = campagne(args.tours, args.port, archive)
    manquantes = [
        i["nom"] for i in donnees["images"] if i["compressee_octets"] is None
    ]
    if args.json:
        Path(args.json).write_text(
            json.dumps(donnees, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
        print(f"rapport écrit : {args.json}")
    if manquantes:
        print(
            f"taille compressée non mesurable sur : {', '.join(manquantes)}",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
