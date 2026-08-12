#!/usr/bin/env python3
"""Le perimetre declare est une donnee : ce fichier en tire la documentation.

    python3 tests/compat/genere_compat.py             # ecrit docs/compat.md et docs/compat.json
    python3 tests/compat/genere_compat.py --verifie   # echoue si le commite differe (la CI)

# Pourquoi

`docs/compat.md` etait une table tenue a la main, excellente et derivante : la
page de presentation du projet annoncait encore « scroll : pas encore » des
mois apres sa livraison. Une table ecrite a la main ne peut pas etre la source
de verite de trois endroits — la documentation, la page web, et le rapport de
conformance qui doit savoir si un cas echoue porte sur une capacite qu'on
declare tenir.

La source est donc [`compat.yaml`](../../compat.yaml), et trois lecteurs en
decoulent :

  docs/compat.md     ce fichier-ci en genere les **tables d'etat** ; le texte
                     long reste ecrit a la main dans `docs/compat.gabarit.md`,
                     ou un marqueur `<!-- table:id -->` dit ou va chaque table
  docs/compat.json   la meme chose pour un lecteur qui n'a pas d'analyseur YAML
                     (une page web, un script)
  conformance_es.py  croise chaque cas echoue de la suite d'Elastic avec le
                     perimetre declare

# Ce que la validation refuse

Elle n'est pas decorative : elle interdit precisement ce qui rendrait le
fichier inutilisable comme source.

  - un `refuse` sans `motif` ni `raison` — c'est la distinction qui compte,
    entre « je ne sais pas faire » et « je refuse expres » ;
  - un `partiel` qui ne dit ni ce qu'il supporte ni ce qu'il refuse ;
  - deux capacites qui declarent la meme API de la suite d'Elastic : un cas
    echoue serait attribue a l'une ou a l'autre selon l'ordre de lecture ;
  - un identifiant en double (les rapports le citent), un motif d'erreur qui
    ne compile pas, un marqueur de table sans table ou l'inverse.
"""
import difflib
import json
import os
import re
import sys

RACINE = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
SOURCE = os.path.join(RACINE, "compat.yaml")
GABARIT = os.path.join(RACINE, "docs", "compat.gabarit.md")
CIBLE_MD = os.path.join(RACINE, "docs", "compat.md")
CIBLE_JSON = os.path.join(RACINE, "docs", "compat.json")

MARQUEUR = re.compile(r"^<!-- table:([a-z_]+) -->$", re.M)


class Invalide(Exception):
    """compat.yaml ne tient pas ses propres regles."""


# ---------------------------------------------------------------------------
# Lecture et validation
# ---------------------------------------------------------------------------


def charge(chemin=SOURCE):
    import yaml

    with open(chemin) as f:
        doc = yaml.safe_load(f)
    valide(doc)
    return doc


def capacites(doc):
    """Toutes les capacites, dans l'ordre du fichier, avec leur table."""
    for table in doc["tables"]:
        for cap in table["capacites"]:
            yield table, cap


def valide(doc):
    if doc.get("schema") != 1:
        raise Invalide(f"schema {doc.get('schema')} inconnu (ce generateur lit le 1)")
    etats, motifs = doc["etats"], doc["motifs"]
    vus, tables_vues, apis_vues = {}, set(), {}
    for table, cap in capacites(doc):
        if table["id"] in tables_vues and table["capacites"][0] is not cap:
            pass
        tables_vues.add(table["id"])
        cid = cap.get("id")
        if not cid:
            raise Invalide(f"une capacite de [{table['id']}] n'a pas d'identifiant")
        if cid in vus:
            raise Invalide(f"identifiant en double : [{cid}]")
        vus[cid] = cap
        if cap.get("etat") not in etats:
            raise Invalide(f"[{cid}] : etat [{cap.get('etat')}] inconnu")
        if "poids" not in cap:
            raise Invalide(f"[{cid}] : il manque [poids] (null tant qu'il n'est pas mesure)")
        poids = cap["poids"]
        if poids is not None and not (isinstance(poids, int) and 0 <= poids <= 100):
            raise Invalide(f"[{cid}] : poids [{poids}] hors de 0-100")
        if cap["etat"] == "refuse":
            if cap.get("motif") not in motifs:
                raise Invalide(f"[{cid}] : un refus porte un motif ({'/'.join(motifs)})")
            if not (cap.get("raison") or "").strip():
                raise Invalide(f"[{cid}] : un refus porte une raison, en une phrase")
        else:
            if cap.get("motif"):
                raise Invalide(f"[{cid}] : [motif] n'a de sens que sur un refus")
        if cap["etat"] == "partiel" and not (cap.get("parametres") or cap.get("detail")):
            raise Invalide(f"[{cid}] : un partiel dit ce qu'il supporte et ce qu'il refuse")
        conf = cap.get("conformance") or {}
        for api in conf.get("apis") or []:
            if api in apis_vues:
                raise Invalide(f"[{cid}] : l'API [{api}] est deja declaree par "
                               f"[{apis_vues[api]}] — un cas echoue serait attribue "
                               f"au hasard")
            apis_vues[api] = cid
        for motif in conf.get("erreurs") or []:
            try:
                re.compile(motif)
            except re.error as e:
                raise Invalide(f"[{cid}] : motif d'erreur illisible [{motif}] : {e}") from e
    if len(tables_vues) != len(doc["tables"]):
        raise Invalide("deux tables portent le meme identifiant")


# ---------------------------------------------------------------------------
# Rendu markdown
# ---------------------------------------------------------------------------


def echappe(texte):
    """Une barre verticale coupe une cellule de table en deux."""
    return str(texte).replace("|", r"\|")


def rend_parametre(p):
    """Un parametre est soit un nom (rendu en code), soit du texte, plus une note.

    Deux cles plutot qu'une heuristique : « les autres metadonnees » n'est pas
    un nom de parametre, et le deviner d'apres la presence d'un espace se
    tromperait le jour ou un parametre en contient un.
    """
    if isinstance(p, str):
        return f"`{p}`"
    if "texte" in p and "nom" in p:
        raise Invalide(f"un parametre porte [nom] ou [texte], pas les deux : {p}")
    rendu = p["texte"] if "texte" in p else f"`{p['nom']}`"
    return f"{rendu} ({p['note']})" if p.get("note") else rendu


def cellule_detail(doc, cap):
    """La colonne « Detail » : la prose, puis ce qui est supporte et refuse.

    Les listes de parametres ne sont **pas** recopiees dans la prose : elles
    sont la donnee, la prose dit ce qu'elles ne savent pas dire. C'est ce qui
    rend la derive impossible plutot qu'improbable.
    """
    morceaux = []
    if cap["etat"] == "refuse":
        titre = doc["motifs"][cap["motif"]]["titre"]
        morceaux.append(f"**{titre}** — {cap['raison'].strip()}")
    if cap.get("detail"):
        morceaux.append(cap["detail"].strip())
    params = cap.get("parametres") or {}
    for cle, etiquette in (("supportes", "Supporté"), ("refuses", "Refusé")):
        if params.get(cle):
            liste = ", ".join(rend_parametre(p) for p in params[cle])
            morceaux.append(f"{etiquette} : {liste}")
    return ". ".join(m.rstrip(".") for m in morceaux if m)


def rend_table(doc, table):
    entete = table["entete"]
    lignes = [ligne(entete), "|" + "---|" * len(entete)]
    for cap in table["capacites"]:
        icone = doc["etats"][cap["etat"]]["icone"]
        lignes.append(ligne([echappe(" ".join(cap["nom"].split())), icone,
                             echappe(cellule_detail(doc, cap))]))
    return "\n".join(lignes)


def ligne(cellules):
    """Une cellule vide s'ecrit `| |`, pas `|  |` : deux espaces se verraient
    dans le diff sans rien vouloir dire."""
    return ("| " + " | ".join(cellules) + " |").replace("|  |", "| |")


def rend_legende(doc):
    lignes = ["| | |", "|---|---|"]
    for etat in doc["etats"].values():
        lignes.append(f"| {etat['icone']} | {etat['legende'].strip()} |")
    return "\n".join(lignes)


def rend_motifs(doc):
    lignes = ["| Motif | Ce qu'il veut dire |", "|---|---|"]
    for motif in doc["motifs"].values():
        lignes.append(f"| **{motif['titre']}** | {' '.join(motif['legende'].split())} |")
    return "\n".join(lignes)


def rend_markdown(doc, gabarit):
    tables = {t["id"]: t for t in doc["tables"]}
    manquants = []

    def remplace(m):
        nom = m.group(1)
        if nom == "legende":
            return rend_legende(doc)
        if nom == "motifs":
            return rend_motifs(doc)
        if nom not in tables:
            manquants.append(nom)
            return m.group(0)
        return rend_table(doc, tables.pop(nom))

    sortie = MARQUEUR.sub(remplace, gabarit)
    if manquants:
        raise Invalide(f"le gabarit appelle des tables absentes de compat.yaml : {manquants}")
    if tables:
        raise Invalide(f"des tables de compat.yaml ne sont posees nulle part dans le "
                       f"gabarit : {sorted(tables)}")
    return sortie


# ---------------------------------------------------------------------------
# Rendu JSON — la forme que lit une page web, ou un autre script
# ---------------------------------------------------------------------------


BALISES = re.compile(r"`|\*\*|\[([^]]*)\]\([^)]*\)")


def sans_balises(texte):
    """Le nom d'une capacite en texte simple : une page web n'a pas a lire du markdown."""
    return " ".join(BALISES.sub(lambda m: m.group(1) or "", texte).split())


def rend_json(doc):
    liste, totaux = [], {"capacites": 0, "supporte": 0, "partiel": 0, "refuse": 0,
                         "poids_renseignes": 0}
    for table, cap in capacites(doc):
        totaux["capacites"] += 1
        totaux[cap["etat"]] += 1
        totaux["poids_renseignes"] += cap["poids"] is not None
        entree = {
            "id": cap["id"],
            "table": table["id"],
            "nom": " ".join(cap["nom"].split()),
            "nom_texte": sans_balises(cap["nom"]),
            "etat": cap["etat"],
            "poids": cap["poids"],
        }
        if cap.get("detail"):
            entree["detail"] = " ".join(cap["detail"].split())
        if cap["etat"] == "refuse":
            entree["motif"] = cap["motif"]
            entree["raison"] = " ".join(cap["raison"].split())
        params = cap.get("parametres") or {}
        if params:
            entree["parametres"] = {
                cle: [p if isinstance(p, str)
                      else sans_balises(p.get("nom") or p["texte"])
                      for p in params[cle]]
                for cle in ("supportes", "refuses") if params.get(cle)
            }
        liste.append(entree)
    return {
        "schema": doc["schema"],
        "genere_par": "tests/compat/genere_compat.py",
        "source": "compat.yaml",
        "api_cible": doc["api_cible"],
        # Les textes gardent leur markdown : ce fichier est la source, pas un
        # rendu. `nom_texte` est la seule commodite, parce que c'est le champ
        # qu'une page web affiche tel quel.
        "etats": {k: {"icone": v["icone"], "legende": " ".join(v["legende"].split())}
                  for k, v in doc["etats"].items()},
        "motifs": {k: {"titre": v["titre"], "legende": " ".join(v["legende"].split())}
                   for k, v in doc["motifs"].items()},
        "totaux": totaux,
        "capacites": liste,
    }


# ---------------------------------------------------------------------------


def ecris(chemin, contenu, verifie):
    """Ecrit, ou compare — et dans les deux cas dit ce qui differe."""
    ancien = open(chemin).read() if os.path.exists(chemin) else None
    if not verifie:
        with open(chemin, "w") as f:
            f.write(contenu)
        etat = "inchange" if ancien == contenu else "ecrit"
        print(f"  {os.path.relpath(chemin, RACINE):<24} {etat}")
        return True
    if ancien == contenu:
        print(f"  {os.path.relpath(chemin, RACINE):<24} a jour")
        return True
    print(f"\n== {os.path.relpath(chemin, RACINE)} differe de ce que compat.yaml genere :")
    diff = difflib.unified_diff((ancien or "").splitlines(True), contenu.splitlines(True),
                                "commite", "genere")
    sys.stdout.writelines(list(diff)[:200])
    return False


def main():
    verifie = "--verifie" in sys.argv[1:]
    try:
        doc = charge()
    except Invalide as e:
        print(f"compat.yaml : {e}", file=sys.stderr)
        return 2
    except ImportError:
        print("il manque PyYAML : pip install pyyaml", file=sys.stderr)
        return 2

    with open(GABARIT) as f:
        gabarit = f.read()
    try:
        md = rend_markdown(doc, gabarit)
    except Invalide as e:
        print(f"compat.yaml / compat.gabarit.md : {e}", file=sys.stderr)
        return 2
    js = json.dumps(rend_json(doc), indent=2, ensure_ascii=False) + "\n"

    total = sum(len(t["capacites"]) for t in doc["tables"])
    print(f"== compat.yaml : {total} capacites, {len(doc['tables'])} tables")
    ok = ecris(CIBLE_MD, md, verifie) & ecris(CIBLE_JSON, js, verifie)
    if verifie and not ok:
        print("\n   regenere-les dans la meme PR : "
              "python3 tests/compat/genere_compat.py", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
