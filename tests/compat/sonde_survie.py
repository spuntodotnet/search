#!/usr/bin/env python3
"""Sonde : le serveur **survit-il** a la requete ?

Toutes les autres sondes de ce repertoire comparent deux reponses. Celle-ci
pose d'abord une question qui vient avant : *y a-t-il encore quelqu'un pour
repondre ?* Un `panic!` dans ferrite n'est pas une erreur 500 — le profil de
release porte `panic = "abort"`, donc le processus entier meurt et tous les
index qu'il servait deviennent injoignables. Un comparateur de reponses voit
ca comme un ecart de plus ; c'est une classe de defaut a part, et elle merite
son predicat.

Le predicat est donc explicite et il s'applique **apres chaque cas** :

    GET / doit rendre 200

Un cas qui le fait tomber est nomme `MORT`, la sonde relance la cible si elle
sait comment, et le rapport le sort en tete — au lieu de le noyer dans une
liste d'ecarts.

Le second predicat est le predicat habituel du depot : repondre n'est pas
repondre **juste**. Chaque cas est aussi pose a un vrai Elasticsearch, et le
statut, le type d'erreur et sa phrase sont compares. Le prefixe `[ligne:colonne]`
qu'ES place devant ses erreurs de document est retire des deux cotes : il
designe une position dans le JSON brut, que ferrite n'a plus une fois le corps
parse (divergence declaree dans `docs/compat.md`).

    python3 tests/compat/sonde_survie.py [ferrite] [es]
    python3 tests/compat/sonde_survie.py --calibrer [es_a] [es_b]
    FERRITE_CMD='...' python3 tests/compat/sonde_survie.py   # relance apres un MORT

Les cas viennent de deux endroits, et aucun n'a ete invente :

* **les six routes du conflit de chemin** — un objet pose la ou le mapping
  declare une valeur, dans un document, par un `copy_to`, ou par un
  `PUT /_mapping`. C'est le defaut de la carte 42 : `PUT` accepte en 200, un
  seul document, et le serveur meurt ;
* **les valeurs qui se decoupent en octets** — un decalage de date
  `+aéb` faisait paniquer le decoupage de `src/dateformat.rs`, sur les six
  routes qui lisent une borne de date.

Le meme fichier lance contre le binaire d'avant rend 12 cas `MORT`.
"""
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request

# Un caractere multi-octets place de facon que la frontiere tombe au milieu :
# `a` + `é` (deux octets) + `b` fait quatre octets, et l'octet 2 est au milieu
# du `é`. C'est exactement ce qu'il faut pour faire tomber un decoupage qui
# compte en octets sans le dire.
MULTI = "aéb"

PREFIXE_POSITION = re.compile(r"^\[\d+:\d+\] ")


def http(base, method, path, body=None, timeout=15):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        base + path, data=data, method=method,
        headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        corps = e.read()
        try:
            return e.code, json.loads(corps or b"{}")
        except json.JSONDecodeError:
            return e.code, {"raw": corps.decode(errors="replace")}
    except Exception as exc:  # noqa: BLE001 — c'est le cas qui nous interesse
        return None, {"injoignable": repr(exc)}


def vivant(base):
    st, _ = http(base, "GET", "/", timeout=3)
    return st == 200


# ---------------------------------------------------------------------------
# Les cas
#
# Chacun est une liste d'etapes `(methode, chemin, corps)` jouee sur un index
# neuf ; `{i}` y est remplace par le nom de l'index du cas. Seule la **derniere**
# etape est comparee : les precedentes posent l'etat.
# ---------------------------------------------------------------------------
FEUILLE = {"mappings": {"properties": {"a": {"type": "keyword"}}}}
OBJET = {"mappings": {"properties": {"a": {"properties": {"b": {"type": "keyword"}}}}}}
NESTED = {"mappings": {"properties": {
    "a": {"type": "nested", "properties": {"b": {"type": "keyword"}}}}}}


def copie(cible, ty="keyword", dyn=None):
    props = {"a": {"type": ty}, "s": {"type": "keyword", "copy_to": cible}}
    m = {"properties": props}
    if dyn is not None:
        m["dynamic"] = dyn
    return {"mappings": m}


CAS = [
    # ----- un objet pose sur une feuille declaree -------------------------
    ("doc : objet sur une feuille",
     [("PUT", "/{i}", FEUILLE), ("POST", "/{i}/_doc/1", {"a": {"b": "x"}})]),
    ("doc : objet vide sur une feuille",
     [("PUT", "/{i}", FEUILLE), ("POST", "/{i}/_doc/1", {"a": {}})]),
    ("doc : objet a deux niveaux sur une feuille",
     [("PUT", "/{i}", FEUILLE), ("POST", "/{i}/_doc/1", {"a": {"b": {"c": "x"}}})]),
    ("doc : tableau d'objets sur une feuille",
     [("PUT", "/{i}", FEUILLE), ("POST", "/{i}/_doc/1", {"a": [{"b": "x"}]})]),
    ("doc : deux objets, le premier est l'apercu",
     [("PUT", "/{i}", FEUILLE),
      ("POST", "/{i}/_doc/1", {"a": [{"b": "x"}, {"b": "y"}]})]),
    ("doc : objet melange a une valeur",
     [("PUT", "/{i}", FEUILLE), ("POST", "/{i}/_doc/1", {"a": [1, {"b": "x"}]})]),
    ("doc : cles de l'apercu triees",
     [("PUT", "/{i}", FEUILLE),
      ("POST", "/{i}/_doc/1", {"a": {"c": "y", "b": "x"}})]),
    ("doc : apercu d'un tableau et d'un nul",
     [("PUT", "/{i}", FEUILLE),
      ("POST", "/{i}/_doc/1", {"a": {"b": [1, 2], "c": None, "d": True}})]),
    ("doc : objet sur une feuille numerique",
     [("PUT", "/{i}", {"mappings": {"properties": {"a": {"type": "long"}}}}),
      ("POST", "/{i}/_doc/1", {"a": {"b": 1}})]),
    ("doc : objet sur un multi-field",
     [("PUT", "/{i}", {"mappings": {"properties": {
         "a": {"type": "keyword", "fields": {"b": {"type": "text"}}}}}}),
      ("POST", "/{i}/_doc/1", {"a": {"b": "x"}})]),
    ("doc : objet sur une feuille sous un nested",
     [("PUT", "/{i}", NESTED),
      ("POST", "/{i}/_doc/1", {"a": [{"b": {"c": "x"}}]})]),
    # `dynamic` ne change rien : le controle est **avant** lui chez ES.
    ("doc : objet sur une feuille, dynamic false",
     [("PUT", "/{i}", {"mappings": {"dynamic": False, "properties": {
         "a": {"type": "keyword"}}}}),
      ("POST", "/{i}/_doc/1", {"a": {"b": "x"}})]),
    ("doc : objet sur une feuille, dynamic strict",
     [("PUT", "/{i}", {"mappings": {"dynamic": "strict", "properties": {
         "a": {"type": "keyword"}}}}),
      ("POST", "/{i}/_doc/1", {"a": {"b": "x"}})]),
    # ----- l'inverse : une valeur posee sur un objet ----------------------
    ("doc : valeur sur un objet",
     [("PUT", "/{i}", OBJET), ("POST", "/{i}/_doc/1", {"a": "x"})]),
    ("doc : nombre sur un objet",
     [("PUT", "/{i}", OBJET), ("POST", "/{i}/_doc/1", {"a": 1})]),
    ("doc : valeur sur un nested",
     [("PUT", "/{i}", NESTED), ("POST", "/{i}/_doc/1", {"a": "x"})]),
    ("doc : valeur dans un tableau sur un objet",
     [("PUT", "/{i}", OBJET), ("POST", "/{i}/_doc/1", {"a": [1, 2]})]),
    # Ceux-la doivent passer : un objet absent n'est pas un objet mal forme.
    ("doc : nul sur un objet (accepte)",
     [("PUT", "/{i}", OBJET), ("POST", "/{i}/_doc/1", {"a": None})]),
    ("doc : tableau vide sur un objet (accepte)",
     [("PUT", "/{i}", OBJET), ("POST", "/{i}/_doc/1", {"a": []})]),
    ("doc : tableau de nuls sur un objet (accepte)",
     [("PUT", "/{i}", OBJET), ("POST", "/{i}/_doc/1", {"a": [None]})]),
    ("doc : sous-champ inconnu sous un objet (accepte)",
     [("PUT", "/{i}", OBJET), ("POST", "/{i}/_doc/1", {"a": {"z": "y"}})]),
    # ----- la cible d'un copy_to -----------------------------------------
    ("copy_to : sous-chemin d'une feuille",
     [("PUT", "/{i}", copie("a.b")), ("POST", "/{i}/_doc/1", {"s": "x"})]),
    ("copy_to : sous-chemin a deux niveaux",
     [("PUT", "/{i}", copie("a.b.c")), ("POST", "/{i}/_doc/1", {"s": "x"})]),
    ("copy_to : premiere valeur d'un tableau",
     [("PUT", "/{i}", copie("a.b")), ("POST", "/{i}/_doc/1", {"s": ["x", "y"]})]),
    ("copy_to : valeur nulle (refusee quand meme)",
     [("PUT", "/{i}", copie("a.b")), ("POST", "/{i}/_doc/1", {"s": None})]),
    ("copy_to : tableau vide (accepte)",
     [("PUT", "/{i}", copie("a.b")), ("POST", "/{i}/_doc/1", {"s": []})]),
    ("copy_to : champ source absent (accepte)",
     [("PUT", "/{i}", copie("a.b")), ("POST", "/{i}/_doc/1", {"a": "z"})]),
    ("copy_to : sous-chemin de soi-meme",
     [("PUT", "/{i}", {"mappings": {"properties": {
         "s": {"type": "keyword", "copy_to": "s.x"}}}}),
      ("POST", "/{i}/_doc/1", {"s": "x"})]),
    ("copy_to : ancetre de type text",
     [("PUT", "/{i}", copie("a.b", ty="text")),
      ("POST", "/{i}/_doc/1", {"s": "x"})]),
    ("copy_to : ancetre de type date",
     [("PUT", "/{i}", copie("a.b", ty="date")),
      ("POST", "/{i}/_doc/1", {"s": "x"})]),
    ("copy_to : dynamic false ne change rien",
     [("PUT", "/{i}", copie("a.b", dyn=False)),
      ("POST", "/{i}/_doc/1", {"s": "x"})]),
    ("copy_to : dynamic strict ne change rien",
     [("PUT", "/{i}", copie("a.b", dyn="strict")),
      ("POST", "/{i}/_doc/1", {"s": "x"})]),
    # ----- PUT /_mapping --------------------------------------------------
    ("_mapping : un objet par-dessus une feuille",
     [("PUT", "/{i}", FEUILLE),
      ("PUT", "/{i}/_mapping", {"properties": {
          "a": {"properties": {"b": {"type": "keyword"}}}}})]),
    ("_mapping : un chemin pointe par-dessus une feuille",
     [("PUT", "/{i}", FEUILLE),
      ("PUT", "/{i}/_mapping", {"properties": {"a.b": {"type": "keyword"}}})]),
    ("_mapping : une feuille par-dessus un objet",
     [("PUT", "/{i}", OBJET),
      ("PUT", "/{i}/_mapping", {"properties": {"a": {"type": "keyword"}}})]),
    ("_mapping : une feuille par-dessus un nested",
     [("PUT", "/{i}", NESTED),
      ("PUT", "/{i}/_mapping", {"properties": {"a": {"type": "keyword"}}})]),
    # ----- la declaration, qui elle est refusee des le PUT ----------------
    ("PUT : a et a.b declares ensemble",
     [("PUT", "/{i}", {"mappings": {"properties": {
         "a": {"type": "keyword"}, "a.b": {"type": "keyword"}}}})]),
    # ----- une valeur qui ressemble a un numero de champ -------------------
    # `_validate/query?explain=true` rend la requete traduite ; ferrite la lit
    # dans le `Debug` de tantivy et y remplace `field=<n>` par le nom du champ.
    # Le `Debug` transporte aussi la **valeur** cherchee : un client qui tape
    # `field=999999` faisait indexer un tableau hors de ses bornes, donc mourir
    # le processus. Trouve en relisant les points de panique, pas par un client.
    ("_validate/query : une valeur qui contient field=999999",
     [("PUT", "/{i}", {"mappings": {"properties": {"k": {"type": "keyword"}}}}),
      ("POST", "/{i}/_validate/query?explain=true",
       {"query": {"term": {"k": "field=999999"}}})]),
    ("_validate/query : une valeur qui contient field=0",
     [("PUT", "/{i}", {"mappings": {"properties": {"k": {"type": "keyword"}}}}),
      ("POST", "/{i}/_validate/query?explain=true",
       {"query": {"term": {"k": "field=0"}}})]),
]

# La borne de date : le meme defaut, atteint par les six routes qui lisent une
# requete plus l'ecriture d'un document.
#
# Ces cas-la se comparent sur le **statut et le type d'erreur**, pas sur la
# phrase, et la raison est ecrite : la phrase d'un echec de lecture de date
# diverge deja d'ES, avant cette carte et pour toutes les dates — ES rend
# `failed to parse field [d] of type [date] ... Preview of field's value` la ou
# ferrite rend `failed to parse date field [d] with value [...]`. Ce que ces cas
# mesurent est ailleurs : le serveur repond-il, et refuse-t-il la ou ES refuse ?
#
# Un document est indexe d'abord, et ce n'est pas decoratif : sur un index
# **vide**, ES ne lit meme pas la borne (`_search` y rend 200 et zero document,
# mesure), donc la comparaison ne dirait rien.
MAP_DATE = {"mappings": {"properties": {"d": {"type": "date"}}}}
POSE_DOC = ("POST", "/{i}/_doc/1?refresh=true", {"d": "2020-01-01"})


def borne(route, methode="POST", corps=None):
    q = {"query": {"range": {"d": {"gte": "2020-01-01T00:00:00+" + MULTI}}}}
    return [("PUT", "/{i}", MAP_DATE), POSE_DOC,
            (methode, "/{i}" + route, corps if corps is not None else q)]


CAS_STATUT = [
    ("date : decalage multi-octets a l'ecriture",
     [("PUT", "/{i}", MAP_DATE),
      ("POST", "/{i}/_doc/1", {"d": "2020-01-01T00:00:00+" + MULTI})]),
    ("date : decalage a cinq octets dont un accent",
     [("PUT", "/{i}", MAP_DATE),
      ("POST", "/{i}/_doc/1", {"d": "2020-01-01T00:00:00+é:00"})]),
    ("date : decalage multi-octets dans un _search", borne("/_search")),
    ("date : decalage multi-octets dans un _count", borne("/_count")),
    ("date : decalage multi-octets dans un _validate/query",
     borne("/_validate/query")),
    ("date : decalage multi-octets dans un _delete_by_query",
     borne("/_delete_by_query")),
    ("date : decalage multi-octets dans un index_filter",
     borne("/_field_caps?fields=*", corps={"index_filter": {
         "range": {"d": {"gte": "2020-01-01T00:00:00+" + MULTI}}}})),
    ("date : decalage multi-octets dans un _explain",
     borne("/_explain/1", methode="GET")),
]


def type_erreur(body):
    err = body.get("error")
    if not isinstance(err, dict):
        return ""
    racines = err.get("root_cause") or []
    if racines and isinstance(racines[0], dict) and racines[0].get("type"):
        return racines[0]["type"]
    return err.get("type", "")


def raison(body):
    err = body.get("error")
    if not isinstance(err, dict):
        return ""
    return PREFIXE_POSITION.sub("", err.get("reason", ""))


def comparable(st, body, phrase=True):
    if st is None:
        return "INJOIGNABLE"
    if st >= 400:
        return (f"{st} {type_erreur(body)} : {raison(body)}" if phrase
                else f"{st} {type_erreur(body)}")
    return str(st)


def relance(cmd):
    """Relance la cible si on sait comment — sinon la sonde s'arrete."""
    if not cmd:
        return False
    subprocess.Popen(cmd, shell=True, stdout=subprocess.DEVNULL,
                     stderr=subprocess.DEVNULL)
    return True


def main():
    argv = [a for a in sys.argv[1:] if not a.startswith("--")]
    calibrer = "--calibrer" in sys.argv
    a = argv[0] if argv else ("http://localhost:9201" if calibrer
                              else "http://localhost:9200")
    b = argv[1] if len(argv) > 1 else "http://localhost:9201"
    if calibrer and a == b:
        print("# --calibrer demande deux Elasticsearch differents")
        return 2
    noms = ("es_a", "es_b") if calibrer else ("ferrite", "es")
    cmd = os.environ.get("FERRITE_CMD", "")

    for nom, base in zip(noms, (a, b)):
        if not vivant(base):
            print(f"# {nom} indisponible ({base}) — une sonde differentielle "
                  f"qui ne compare rien ne rend pas de verdict")
            return 2

    morts, ecarts, total = [], [], 0
    tous = [(c, True) for c in CAS] + [(c, False) for c in CAS_STATUT]
    for k, ((libelle, etapes), phrase) in enumerate(tous):
        if k == len(CAS):
            print("\n# la borne de date — compares sur le statut et le type "
                  "d'erreur, pas sur la phrase (raison en tete de fichier)\n")
        idx = f"survie-{k:03d}"
        vus = []
        for nom, base in zip(noms, (a, b)):
            http(base, "DELETE", "/" + idx)
            derniere = (None, {})
            for methode, chemin, corps in etapes:
                derniere = http(base, methode, chemin.format(i=idx), corps)
            vus.append((nom, comparable(*derniere, phrase=phrase)))
            http(base, "DELETE", "/" + idx)
        total += 1

        cible_morte = not vivant(a)
        if cible_morte:
            morts.append(libelle)
            marque = "MORT "
            if relance(cmd):
                for _ in range(40):
                    time.sleep(0.25)
                    if vivant(a):
                        break
        elif len({v for _, v in vus}) > 1:
            ecarts.append(libelle)
            marque = "  *  "
        else:
            marque = "     "
        print(f"{marque} {libelle:46} " +
              "  |  ".join(f"{n}={v[:96]}" for n, v in vus))
        if cible_morte and not vivant(a):
            print(f"\n# {a} est mort sur « {libelle} » et n'a pas ete relance "
                  f"(FERRITE_CMD n'est pas pose) : la campagne s'arrete ici")
            break

    print(f"\n{total - len(morts) - len(ecarts)}/{total} identiques, "
          f"{len(ecarts)} ecarts, {len(morts)} cas ou la cible est MORTE")
    for m in morts:
        print(f"  MORT : {m}")
    for e in ecarts:
        print(f"  ecart : {e}")
    return 1 if (morts or ecarts) else 0


if __name__ == "__main__":
    sys.exit(main())
