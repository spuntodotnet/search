#!/usr/bin/env python3
"""Les trois tailles d'une image Docker, comptees sur ses octets.

Lit l'archive OCI produite par ``docker save`` (chemin dans ``$IMAGE_TAR``) et
rend, pour la plateforme locale :

- **compressee (registre)** : la somme des blobs qu'un registre sert pour cette
  image — le manifeste, la configuration, les couches telles qu'elles sont
  poussees. C'est ce qu'un ``docker pull`` telecharge ;
- **decompressee (disque)** : la somme des couches une fois depliees, c'est-a-dire
  ce que le systeme de fichiers de l'image occupe ;
- **binaire de l'ENTRYPOINT** : la taille du fichier que le conteneur execute.
  Sur une image ``scratch`` qui ne contient que lui, c'est presque toute la
  ligne precedente ; sur une image qui demarre une JVM, ca ne veut rien dire, et
  c'est imprime tel quel plutot que compare.

Aucun champ dont la definition depend de la version de Docker n'est lu :
``docker image inspect --format '{{.Size}}'`` rendait la taille decompressee
jusqu'a Docker 28 et rend la taille compressee depuis Docker 29. C'est
precisement l'ambiguite que ce fichier existe pour lever.
"""

from __future__ import annotations

import io
import json
import os
import subprocess
import sys
import tarfile


class Compteur(io.RawIOBase):
    """Un flux qui compte ce qui le traverse."""

    def __init__(self, source):
        self.source = source
        self.octets = 0

    def readable(self):
        return True

    def readinto(self, b):
        morceau = self.source.read(len(b))
        if not morceau:
            return 0
        b[: len(morceau)] = morceau
        self.octets += len(morceau)
        return len(morceau)


def flux_decompresse(brut, media_type: str):
    """Rend un flux lisible sur la couche decompressee.

    La compression est reconnue aux octets, pas au ``mediaType`` : celui d'une
    couche gzip s'ecrit ``…tar+gzip`` en OCI et ``…tar.gzip`` en schema 2 de
    Docker, et se fier au premier fait passer les images poussees par Docker
    pour deja decompressees — 669 Mo au lieu de 1,28 Go sur Elasticsearch 8.15,
    sans un mot.
    """
    entete = brut.read(4)
    brut.seek(0)
    if entete[:2] == b"\x1f\x8b":
        media_type = "+gzip"
    elif entete == b"\x28\xb5\x2f\xfd":
        media_type = "+zstd"
    elif media_type.endswith(("+gzip", ".gzip", "+zstd", ".zstd")):
        raise SystemExit(
            f"couche annoncee {media_type} mais dont les octets ne le sont pas"
        )
    else:
        media_type = ""

    if media_type.endswith("+gzip"):
        import gzip

        return gzip.GzipFile(fileobj=brut)
    if media_type.endswith("+zstd"):
        try:
            from compression import zstd  # Python >= 3.14

            return zstd.ZstdFile(brut)
        except ImportError:
            pass
        try:
            import zstandard

            return zstandard.ZstdDecompressor().stream_reader(brut)
        except ImportError:
            pass
        proc = subprocess.Popen(
            ["zstd", "-dc"], stdin=subprocess.PIPE, stdout=subprocess.PIPE
        )
        # Ecriture puis lecture : les couches tiennent en memoire cote pipe
        # seulement si on les consomme, donc on passe par un fichier temporaire.
        import tempfile

        with tempfile.TemporaryFile() as tmp:
            proc.stdin.write(brut.read())
            proc.stdin.close()
            while True:
                bloc = proc.stdout.read(1 << 20)
                if not bloc:
                    break
                tmp.write(bloc)
            proc.wait()
            tmp.seek(0)
            return io.BytesIO(tmp.read())
    return brut


def mo(octets: int) -> str:
    """Megaoctets decimaux, comme ceux qu'affiche `docker images`."""
    return f"{octets / 1_000_000:.1f} Mo".replace(".", ",")


def espace(octets: int) -> str:
    return f"{octets:,}".replace(",", " ")


def plateforme_locale() -> tuple[str, str]:
    arch = subprocess.run(
        ["docker", "version", "--format", "{{.Server.Arch}}"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    os_ = subprocess.run(
        ["docker", "version", "--format", "{{.Server.Os}}"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    return os_, arch


def blob(tar: tarfile.TarFile, digest: str):
    nom = "blobs/" + digest.replace(":", "/")
    fichier = tar.extractfile(nom)
    if fichier is None:
        raise SystemExit(f"blob absent de l'archive : {nom}")
    return fichier


def taille_blob(tar: tarfile.TarFile, digest: str) -> int:
    return tar.getmember("blobs/" + digest.replace(":", "/")).size


def descend(tar: tarfile.TarFile, descripteur: dict, os_: str, arch: str) -> dict:
    """Suit les index imbriques jusqu'au manifeste de la plateforme locale."""
    with blob(tar, descripteur["digest"]) as f:
        doc = json.load(f)
    if "manifests" not in doc:
        return {"descripteur": descripteur, "manifeste": doc}
    candidats = []
    for m in doc["manifests"]:
        if m.get("annotations", {}).get("vnd.docker.reference.type") == "attestation-manifest":
            continue  # une attestation n'est pas servie par un `docker pull`
        p = m.get("platform", {})
        if p.get("os") in (None, os_) and p.get("architecture") in (None, arch):
            candidats.append(m)
    if not candidats:
        candidats = [m for m in doc["manifests"] if "annotations" not in m]
    if not candidats:
        raise SystemExit(f"aucun manifeste pour {os_}/{arch}")
    return descend(tar, candidats[0], os_, arch)


def main() -> int:
    chemin = os.environ.get("IMAGE_TAR")
    if not chemin:
        print("IMAGE_TAR manquant (chemin de l'archive `docker save`)", file=sys.stderr)
        return 2
    nom_image = sys.argv[1] if len(sys.argv) > 1 else chemin
    os_, arch = plateforme_locale()

    with tarfile.open(chemin) as tar:
        if "index.json" not in tar.getnames():
            # `docker save` a produit une archive « docker-archive » : le magasin
            # d'images classique n'y met que les couches DECOMPRESSEES. La taille
            # compressee — celle qu'un registre sert, donc le chiffre publie —
            # n'y est pas, et l'inventer en recompressant ici mesurerait notre
            # gzip, pas celui que le registre servirait. Donc on refuse.
            raise SystemExit(
                "archive au format docker-archive : elle ne porte pas les blobs "
                "compresses, donc la taille compressee n'est pas mesurable.\n"
                "Deux facons de l'obtenir :\n"
                "  - activer le magasin d'images containerd (defaut depuis Docker 29) ;\n"
                "  - produire l'artefact OCI directement, ce qui est exactement ce "
                "qu'un `docker push` enverrait :\n"
                "      docker buildx build --output type=oci,dest=image.tar .\n"
                "      IMAGE_TAR=image.tar python3 tests/compat/tailles_image.py ferrite"
            )
        index = json.load(tar.extractfile("index.json"))
        trouve = descend(tar, index["manifests"][0], os_, arch)
        manifeste = trouve["manifeste"]
        descripteur = trouve["descripteur"]

        compressee = descripteur["size"] + manifeste["config"]["size"]
        compressee += sum(c["size"] for c in manifeste["layers"])

        entrypoint = None
        with blob(tar, manifeste["config"]["digest"]) as f:
            config = json.load(f)
        commande = config.get("config", {}).get("Entrypoint") or config.get(
            "config", {}
        ).get("Cmd") or []
        if commande:
            entrypoint = commande[0].lstrip("/")

        decompressee = 0
        taille_entrypoint = None
        for couche in manifeste["layers"]:
            with blob(tar, couche["digest"]) as brut:
                flux = flux_decompresse(brut, couche["mediaType"])
                compteur = Compteur(flux)
                enveloppe = io.BufferedReader(compteur)
                if entrypoint is None:
                    while enveloppe.read(1 << 20):
                        pass
                else:
                    with tarfile.open(fileobj=enveloppe, mode="r|*") as couche_tar:
                        for membre in couche_tar:
                            if membre.name.lstrip("./") == entrypoint and membre.isfile():
                                taille_entrypoint = membre.size
                    while enveloppe.read(1 << 20):
                        pass
                decompressee += compteur.octets

    print(f"== image : {nom_image}  ({os_}/{arch}, {len(manifeste['layers'])} couche(s))")
    lignes = [
        (
            "compressee (registre)",
            compressee,
            "ce qu'un `docker pull` telecharge",
        ),
        (
            "decompressee (disque)",
            decompressee,
            "ce que le systeme de fichiers de l'image occupe",
        ),
    ]
    if taille_entrypoint is not None:
        lignes.append(
            (
                f"binaire /{entrypoint}",
                taille_entrypoint,
                "le fichier que le conteneur execute",
            )
        )
    largeur = max(len(nom) for nom, _, _ in lignes)
    for nom, valeur, quoi in lignes:
        print(f"{nom.ljust(largeur)} : {espace(valeur).rjust(13)} octets  {mo(valeur).rjust(9)}   <- {quoi}")
    if entrypoint is not None and taille_entrypoint is None:
        # Ne rien imprimer se lirait « pas de binaire », donc « image vide ».
        print(
            f"{'binaire /' + entrypoint:{largeur}} : non mesure — aucune couche ne porte ce"
            " chemin en fichier ordinaire (lien symbolique, ou chemin traversant un lien)"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
