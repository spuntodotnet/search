#!/usr/bin/env python3
"""Le tantivy epingle est-il bien 0.26.1 **plus un seul fichier** ?

    python3 tests/compat/verifie_tantivy.py

# Pourquoi ce fichier existe

`Cargo.toml` ne prend plus tantivy sur crates.io : il l'epingle sur un commit
d'un fork, parce que 0.26.1 perd les documents des buckets rares d'une
sous-agregation (voir [`docs/tantivy-patch.md`](../../docs/tantivy-patch.md)).
Une epingle comme celle-la demande qu'on croie sur parole neuf caisses de code
tiers — et « c'est juste le correctif d'amont » est exactement le genre de
phrase que ce depot n'accepte pas sans mesure.

Ce script la remplace par une comparaison : il telecharge les **crates
publiees** sur crates.io pour chaque paquet que l'epingle remplace, extrait
l'arbre du fork au commit epingle, et compare fichier par fichier. Le seul
ecart tolere est ecrit ci-dessous, en dur. Tout le reste — un fichier de plus,
un fichier de moins, un octet qui bouge ailleurs — fait echouer.

Il ne dit rien de la *justesse* du correctif : c'est le travail de
`tests/spike_sous_aggs.rs` (le comportement) et de
`tests/compat/sonde_sous_aggs.py` (la comparaison a un vrai Elasticsearch).
Celui-ci ne repond qu'a une question : **qu'est-ce qu'on a vraiment epingle ?**
"""
import hashlib
import io
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path

RACINE = Path(__file__).resolve().parents[2]

# Le seul ecart attendu entre le fork epingle et les crates publiees. Une ligne
# de plus ici est une decision, pas un detail : elle doit etre justifiee dans
# docs/tantivy-patch.md.
ECARTS_ATTENDUS = {("tantivy", "src/aggregation/cached_sub_aggs.rs")}

# Ce que cargo ajoute ou normalise en publiant : ce ne sont pas des sources.
IGNORES = {".cargo-ok", ".cargo_vcs_info.json", "Cargo.toml", "Cargo.toml.orig",
           "Cargo.lock"}


def epingle():
    """Le depot et le commit epingles, lus dans Cargo.toml."""
    texte = (RACINE / "Cargo.toml").read_text()
    m = re.search(r'^\s*tantivy\s*=\s*\{\s*git\s*=\s*"([^"]+)"\s*,\s*'
                  r'rev\s*=\s*"([0-9a-f]{40})"\s*\}', texte, re.M)
    if not m:
        raise SystemExit("  !! aucun [patch.crates-io] tantivy = { git, rev } "
                         "dans Cargo.toml. Si l'epingle a ete retiree parce que "
                         "le correctif est publie, retirer ce script du CI dans "
                         "la meme PR.")
    return m.group(1), m.group(2)


def paquets_epingles(rev):
    """Les paquets que Cargo.lock prend sur le fork, avec leur version.

    C'est Cargo.lock qui fait foi, pas une liste ecrite ici : epingler
    `tantivy` entraine avec lui les crates de son espace de travail, et cette
    liste-la peut changer sans qu'on la retape."""
    out = {}
    nom = version = None
    for ligne in (RACINE / "Cargo.lock").read_text().splitlines():
        m = re.match(r'^(name|version|source) = "([^"]*)"$', ligne)
        if not m:
            continue
        if m.group(1) == "name":
            nom, version = m.group(2), None
        elif m.group(1) == "version":
            version = m.group(2)
        elif rev in m.group(2):
            out[nom] = version
    return out


def arbre_du_fork(url, rev, dest):
    """L'arbre du fork au commit epingle, sans son historique."""
    subprocess.run(["git", "init", "-q", str(dest)], check=True)
    subprocess.run(["git", "-C", str(dest), "remote", "add", "origin", url],
                   check=True)
    subprocess.run(["git", "-C", str(dest), "fetch", "-q", "--depth", "1",
                    "origin", rev], check=True)
    subprocess.run(["git", "-C", str(dest), "checkout", "-q", "FETCH_HEAD"],
                   check=True)
    shutil.rmtree(dest / ".git")


def crate_publiee(nom, version, dest):
    """Le contenu de la crate telle que crates.io la sert."""
    url = f"https://static.crates.io/crates/{nom}/{nom}-{version}.crate"
    with urllib.request.urlopen(url, timeout=300) as r:
        brut = r.read()
    with tarfile.open(fileobj=io.BytesIO(brut), mode="r:gz") as t:
        t.extractall(dest, filter="data")
    return dest / f"{nom}-{version}", hashlib.sha256(brut).hexdigest()


def repertoire_du_paquet(racine, nom):
    """Ou vit ce paquet dans l'arbre du fork (lu, pas devine)."""
    for cargo in [racine / "Cargo.toml"] + sorted(racine.glob("*/Cargo.toml")):
        if re.search(rf'^name\s*=\s*"{re.escape(nom)}"\s*$',
                     cargo.read_text(), re.M):
            return cargo.parent
    return None


def fichiers(racine):
    return {str(p.relative_to(racine)): p for p in racine.rglob("*")
            if p.is_file() and p.name not in IGNORES}


def compare(publiee, fork):
    """Les fichiers de la crate **publiee** qui manquent ou different dans le fork.

    La comparaison va dans ce sens-la et pas dans l'autre, exprès. Un fichier
    present dans le fork et absent de la crate publiee ne change pas ce que le
    compilateur produit tant qu'aucun fichier publie ne le cite — et un fichier
    publie qui se mettrait a citer quelque chose de nouveau serait, lui,
    different. Comparer « tout ce qui est dans le depot » ferait au contraire
    du bruit a chaque ligne de son outillage (`.github/`, `Makefile`, `doc/`,
    les autres crates de l'espace de travail), qui n'entre dans aucune crate."""
    a, b = fichiers(publiee), fichiers(fork)
    return sorted(c for c in a
                  if c not in b or a[c].read_bytes() != b[c].read_bytes())


def main():
    url, rev = epingle()
    paquets = paquets_epingles(rev)
    if not paquets:
        raise SystemExit(f"  !! Cargo.lock ne prend aucun paquet sur {rev} : "
                         f"l'epingle et le verrou ne disent pas la meme chose.")
    print(f"== {url}\n   commit {rev}\n   {len(paquets)} paquets pris sur le "
          f"fork au lieu de crates.io\n")

    ecarts, manques, sommes = set(), [], {}
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        fork = tmp / "fork"
        arbre_du_fork(url, rev, fork)

        for nom, version in sorted(paquets.items()):
            rep = repertoire_du_paquet(fork, nom)
            if rep is None:
                manques.append(f"{nom} : aucun Cargo.toml de ce nom dans le fork")
                continue
            pub, somme = crate_publiee(nom, version, tmp / nom)
            sommes[f"{nom}-{version}"] = somme

            differents = compare(pub, rep)
            if differents:
                for c in differents:
                    ecarts.add((nom, c))
                print(f"  {nom} {version} : {len(differents)} fichier(s) "
                      f"different(s) de la crate publiee")
                for c in differents:
                    print(f"      {c}")
            else:
                print(f"  {nom} {version} : identique a la crate publiee")

    print()
    for m in manques:
        print(f"  !! {m}")
    inattendus = ecarts - ECARTS_ATTENDUS
    absents = ECARTS_ATTENDUS - ecarts
    for nom, c in sorted(inattendus):
        print(f"  !! ecart NON declare : {nom} / {c}")
    for nom, c in sorted(absents):
        print(f"  !! l'ecart declare {nom} / {c} n'existe plus : le correctif "
              f"a-t-il ete perdu ? (tests/spike_sous_aggs.rs le dira)")

    if manques or inattendus or absents:
        print(f"\n  ECHEC : l'epingle ne contient pas ce qu'elle declare.")
        return 1
    print(f"  Le fork epingle est {len(paquets)} crates publiees a l'identique, "
          f"plus exactement {len(ECARTS_ATTENDUS)} fichier :")
    for nom, c in sorted(ECARTS_ATTENDUS):
        print(f"      {nom} / {c}")
    print("\n  Empreintes des crates publiees comparees :")
    for cle, somme in sorted(sommes.items()):
        print(f"      {somme}  {cle}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
