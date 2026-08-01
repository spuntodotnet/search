#!/usr/bin/env python3
"""Faire passer a ferrite la suite de conformance **d'Elasticsearch lui-meme**.

    python3 tests/compat/conformance_es.py [URL] [--suites search,get,...] [--verbeux]

Le harnais de ce repo compare ferrite a un vrai ES sur des cas qu'on a ecrits.
Celui-ci change de nature : les cas viennent d'**Elastic**, pas de nous. C'est
la seule facon d'attraper ce qu'on ne sait pas qu'on ignore — un test qu'on
ecrit soi-meme porte la meme idee fausse que le code qu'il teste.

# D'ou viennent ces tests, et pourquoi on a le droit

Elasticsearch publie ses tests REST au format YAML : une suite d'appels et
d'assertions, concue pour etre rejouee par n'importe quel client contre
n'importe quel serveur. **La version 7.10.2 est la derniere publiee sous licence
Apache 2.0** (la 7.11 bascule en SSPL + Elastic License) : c'est donc celle-la
qu'on utilise, et elle est compatible avec la licence de ferrite. Heureuse
coincidence, c'est aussi la version du projet qu'on cherche a servir.

Les fichiers ne sont pas recopies dans ce depot : ils sont telecharges a la
demande dans `.es-rest-spec/` (ignore par git). Provenance et licence restent
donc chez Elastic, et ce fichier n'en contient aucune ligne.

# Ce que le runner sait faire

Le vocabulaire de ces tests est petit : `do`, `match`, `length`, `is_true`,
`is_false`, `gt/gte/lt/lte`, `set`, `skip`, plus `setup` / `teardown`. Les URL
sont resolues depuis les specs d'API (`api/*.json`) du meme depot, donc rien
n'est code en dur ici.

# Comment lire le resultat

Trois colonnes, et c'est la distinction qui compte :

  reussis      ferrite repond comme Elasticsearch
  refuses      ferrite refuse explicitement (hors perimetre) — attendu, pas un bug
  echecs       ferrite repond, mais autre chose : **ce sont les vrais**

Le runner lui-meme se verifie en le lancant contre un vrai Elasticsearch 7.10.2 :
il doit y etre pratiquement tout vert. Un runner qui echoue partout contre ES ne
prouve rien sur ferrite.
"""
import json
import os
import re
import sys
import urllib.error
import urllib.request

RACINE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..")
SPEC_DIR = os.path.abspath(os.path.join(RACINE, ".es-rest-spec"))
VERSION = "7.10.2"
TARBALL = f"https://github.com/elastic/elasticsearch/archive/refs/tags/v{VERSION}.tar.gz"

# Les suites qui interrogent ce que ferrite pretend savoir faire. Les autres
# (snapshots, ILM, cluster distribue, scripts...) sont hors perimetre declare :
# les lancer ne mesurerait rien.
SUITES = [
    "search", "search.aggregation", "count", "index", "create", "get",
    "get_source", "exists", "delete", "update", "bulk", "mget",
    "indices.create", "indices.delete", "indices.exists", "indices.get",
    "indices.get_mapping", "indices.put_mapping", "indices.refresh",
    "cluster.health", "cat.indices", "ping", "info",
]

ARGS = [a for a in sys.argv[1:] if not a.startswith("-")]
URL = ARGS[0] if ARGS else "http://localhost:9200"
VERBEUX = "--verbeux" in sys.argv
if "--suites" in sys.argv:
    SUITES = sys.argv[sys.argv.index("--suites") + 1].split(",")

# Le type d'erreur par lequel ferrite dit « Elasticsearch sait faire, moi pas ».
REFUS_FERRITE = "not_implemented_in_ferrite_exception"


# ---------------------------------------------------------------------------
# Recuperation de la suite
# ---------------------------------------------------------------------------


def assure_spec():
    if os.path.isdir(os.path.join(SPEC_DIR, "test")):
        return
    import io
    import tarfile

    print(f"== telechargement de la suite REST d'Elasticsearch {VERSION} "
          f"(Apache 2.0) -> {SPEC_DIR}")
    with urllib.request.urlopen(TARBALL, timeout=600) as r:
        brut = r.read()
    prefixe = f"elasticsearch-{VERSION}/rest-api-spec/src/main/resources/rest-api-spec/"
    os.makedirs(SPEC_DIR, exist_ok=True)
    with tarfile.open(fileobj=io.BytesIO(brut), mode="r:gz") as t:
        for membre in t.getmembers():
            if not membre.name.startswith(prefixe) or not membre.isfile():
                continue
            rel = membre.name[len(prefixe):]
            cible = os.path.join(SPEC_DIR, rel)
            os.makedirs(os.path.dirname(cible), exist_ok=True)
            with open(cible, "wb") as f:
                f.write(t.extractfile(membre).read())
    print(f"   {sum(len(f) for _, _, f in os.walk(SPEC_DIR))} fichiers")


# ---------------------------------------------------------------------------
# Appels HTTP, guides par les specs d'API
# ---------------------------------------------------------------------------


def decode(brut, content_type):
    """Le corps d'une reponse : du JSON, ou du texte (les `_cat` sans format)."""
    if not brut:
        # Un corps vide en texte est une chaine vide, pas une absence : les
        # assertions `_cat` disent `match: {$body: /^$/}`.
        return None if "json" in content_type else ""
    if "json" not in content_type:
        return brut.decode(errors="replace")
    try:
        return json.loads(brut)
    except json.JSONDecodeError:
        return brut.decode(errors="replace")


class Serveur:
    def __init__(self, base):
        self.base = base.rstrip("/")
        self.api = {}
        for nom in os.listdir(os.path.join(SPEC_DIR, "api")):
            if not nom.endswith(".json") or nom.startswith("_"):
                continue
            with open(os.path.join(SPEC_DIR, "api", nom)) as f:
                self.api.update(json.load(f))

    def url_de(self, api, params):
        """Choisit le chemin le plus specifique dont toutes les parties sont la."""
        spec = self.api[api]["url"]["paths"]
        candidats = []
        for chemin in spec:
            parts = chemin.get("parts", {})
            if all(p in params for p in parts):
                candidats.append((len(parts), chemin))
        if not candidats:
            raise KeyError(f"aucun chemin pour [{api}] avec {sorted(params)}")
        _, choisi = max(candidats, key=lambda c: c[0])
        url = choisi["path"]
        for nom in choisi.get("parts", {}):
            v = params[nom]
            v = ",".join(str(x) for x in v) if isinstance(v, list) else str(v)
            url = url.replace("{" + nom + "}", urllib.request.quote(v, safe=",*"))
        reste = {k: v for k, v in params.items() if k not in choisi.get("parts", {})}
        methode = choisi["methods"][0]
        return url, methode, reste

    def appelle(self, api, params):
        params = dict(params)
        corps = params.pop("body", None)
        # `ignore` appartient au runner officiel (« ne leve pas sur ce statut »),
        # pas a l'API : il ne doit pas partir en query string.
        tolere = params.pop("ignore", None)
        tolere = [tolere] if isinstance(tolere, int) else (tolere or [])
        url, methode, qs = self.url_de(api, params)
        if corps is not None and methode == "GET":
            methode = "POST"
        query = []
        for k, v in qs.items():
            if isinstance(v, bool):
                v = "true" if v else "false"
            elif isinstance(v, (list, tuple)):
                v = ",".join(str(x) for x in v)
            query.append(f"{k}={urllib.request.quote(str(v), safe=',*')}")
        if query:
            url += "?" + "&".join(query)

        entete = {"Content-Type": "application/json"}
        data = None
        if isinstance(corps, list):  # NDJSON : _bulk, _msearch
            lignes = [x if isinstance(x, str) else json.dumps(x, default=str)
                      for x in corps]
            data = ("\n".join(lignes) + "\n").encode()
            entete["Content-Type"] = "application/x-ndjson"
        elif corps is not None:
            data = (corps.encode() if isinstance(corps, str)
                    else json.dumps(corps, default=str).encode())

        req = urllib.request.Request(self.base + url, data=data, method=methode,
                                     headers=entete)
        # Une API en HEAD ne rend pas de corps : sa reponse *est* le booleen,
        # et un 404 y est une reponse, pas une erreur.
        tete = methode == "HEAD"
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                brut = r.read()
                if tete:
                    return r.status, True
                return r.status, decode(brut, r.headers.get("Content-Type", ""))
        except urllib.error.HTTPError as e:
            brut = e.read()
            if tete:
                return (200 if e.code in tolere else e.code), False
            if e.code in tolere:
                return 200, decode(brut, e.headers.get("Content-Type", ""))
            return e.code, decode(brut, e.headers.get("Content-Type", ""))


# ---------------------------------------------------------------------------
# Le runner
# ---------------------------------------------------------------------------

CATCH = {
    "missing": 404,
    "conflict": 409,
    "request": 500,
    "bad_request": 400,
    "param": 400,
    "unauthorized": 401,
    "forbidden": 403,
    "request_timeout": 408,
    "unavailable": 503,
}
# Les fonctionnalites du runner officiel qu'on n'implemente pas : un test qui
# les exige est saute, et compte comme tel.
FEATURES_CONNUES = {"stash_in_path", "default_shards"}


# La suite est celle de la 7.10.2, et ses bornes `skip: version` sont ecrites
# pour un serveur de cette version : c'est donc celle qu'on evalue, quel que
# soit le numero annonce par la cible.
VERSION_EVALUEE = (7, 10, 2)


def _num(v, defaut):
    v = v.strip()
    if not v:
        return defaut
    morceaux = [int(x) for x in re.findall(r"\d+", v)][:3]
    while len(morceaux) < 3:
        morceaux.append(0)
    return tuple(morceaux)


def dans_la_plage(plage):
    """`skip: {version: "7.0.0 - 7.9.99"}` s'applique-t-il a notre version ?"""
    if plage.strip() == "all":
        return True
    for morceau in str(plage).split(","):
        bas, _, haut = morceau.partition("-")
        if _num(bas, (0, 0, 0)) <= VERSION_EVALUEE <= _num(haut, (99, 99, 99)):
            return True
    return False


class Saute(Exception):
    pass


class Echec(Exception):
    pass


class Refus(Exception):
    """ferrite dit explicitement qu'il ne sait pas faire."""


def chemin_de(valeur, chemin):
    if chemin in ("", "$body"):
        return valeur
    courant = valeur
    for morceau in re.split(r"(?<!\\)\.", chemin):
        morceau = morceau.replace("\\.", ".")
        if courant is None:
            return None
        if isinstance(courant, list):
            courant = courant[int(morceau)] if morceau.lstrip("-").isdigit() else None
        elif isinstance(courant, dict):
            courant = courant.get(morceau)
        else:
            return None
    return courant


def resous(valeur, pile):
    """Remplace `$var` et `${var}` par ce qui a ete stocke par `set`."""
    if isinstance(valeur, str) and valeur.startswith("$"):
        cle = valeur[1:].strip("{}")
        if cle in pile:
            return pile[cle]
    if isinstance(valeur, dict):
        return {k: resous(v, pile) for k, v in valeur.items()}
    if isinstance(valeur, list):
        return [resous(v, pile) for v in valeur]
    return valeur


def compare(attendu, obtenu):
    # Les motifs sont ecrits en YAML multi-ligne : ils trainent un saut de ligne
    # final qui ne fait pas partie de l'expression.
    if isinstance(attendu, str):
        attendu = attendu.strip() if attendu.strip().startswith("/") else attendu
    if isinstance(attendu, str) and attendu.startswith("/") and attendu.endswith("/"):
        # Le runner officiel compile ces motifs en mode « commentaires » (les
        # espaces du motif sont ignores), ce dont les assertions sur les sorties
        # `_cat` ont besoin — mais d'autres motifs, eux, contiennent de vrais
        # espaces. On accepte les deux lectures plutot que d'en trancher une.
        motif = attendu[1:-1].strip()
        texte = str(obtenu if obtenu is not None else "")
        for mode in (re.X, 0):
            try:
                if re.search(motif, texte, mode):
                    return True
            except re.error:
                pass
        return False
    if isinstance(attendu, float) or isinstance(obtenu, float):
        try:
            return abs(float(attendu) - float(obtenu)) < 1e-6
        except (TypeError, ValueError):
            return False
    return attendu == obtenu


def joue(serveur, actions, pile, corps_precedent=None):
    reponse = corps_precedent
    for action in actions:
        if not isinstance(action, dict) or not action:
            continue
        verbe, arg = next(iter(action.items()))

        if verbe == "skip":
            fonctions = arg.get("features")
            fonctions = [fonctions] if isinstance(fonctions, str) else (fonctions or [])
            inconnues = [f for f in fonctions if f not in FEATURES_CONNUES]
            if inconnues:
                raise Saute(f"exige {inconnues}")
            if "version" in arg:
                if dans_la_plage(arg["version"]):
                    raise Saute(arg.get("reason", f"skip {arg['version']}"))
            continue

        if verbe == "do":
            arg = dict(arg)
            attrape = arg.pop("catch", None)
            arg.pop("headers", None)
            arg.pop("warnings", None)
            arg.pop("allowed_warnings", None)
            arg.pop("node_selector", None)
            if not arg:
                continue
            api, params = next(iter(arg.items()))
            params = resous(params or {}, pile)
            try:
                statut, reponse = serveur.appelle(api, params)
            except KeyError as e:
                # Aucun chemin ne correspond : un vrai client refuserait aussi.
                # C'est une reponse valable quand le test attend une erreur.
                if attrape in ("param", "request", "bad_request"):
                    continue
                raise Echec(str(e)) from e
            if isinstance(reponse, bool):  # API en HEAD : la reponse est le booleen
                if attrape:
                    continue
                continue
            ty = reponse.get("error", {}) if isinstance(reponse, dict) else {}
            ty = ty.get("type") if isinstance(ty, dict) else None
            if ty == REFUS_FERRITE:
                raise Refus((reponse["error"].get("reason") or "")[:150])
            if attrape:
                attendu = CATCH.get(attrape)
                if attendu is None and attrape.startswith("/"):
                    if not compare(attrape, json.dumps(reponse, default=str)):
                        raise Echec(f"[{api}] devait echouer sur {attrape}, a rendu {statut}")
                elif statut != attendu and not (attendu == 400 and 400 <= statut < 500):
                    raise Echec(f"[{api}] devait echouer en {attrape}, a rendu {statut}")
            elif statut >= 400:
                raison = (reponse or {}).get("error")
                raison = raison.get("reason") if isinstance(raison, dict) else raison
                raise Echec(f"[{api}] {statut} : {str(raison)[:200]}")
            continue

        if verbe == "set":
            for chemin, nom in arg.items():
                pile[nom] = chemin_de(reponse, chemin)
            continue

        if verbe in ("match", "length", "is_true", "is_false", "gt", "gte", "lt", "lte",
                     "contains"):
            if isinstance(arg, str):
                chemin, attendu = arg, None
            else:
                chemin, attendu = next(iter(arg.items()))
            obtenu = chemin_de(reponse, chemin) if chemin != "$body" else reponse
            attendu = resous(attendu, pile)
            if verbe == "match" and not compare(attendu, obtenu):
                raise Echec(f"[{chemin}] attendu {attendu!r}, obtenu {obtenu!r}")
            if verbe == "length" and len(obtenu or []) != attendu:
                raise Echec(f"[{chemin}] longueur {len(obtenu or [])} != {attendu}")
            if verbe == "is_true" and (obtenu is None or obtenu is False or obtenu == ""):
                raise Echec(f"[{chemin}] devait etre vrai, vaut {obtenu!r}")
            if verbe == "is_false" and not (obtenu is None or obtenu is False
                                            or obtenu == "" or obtenu == 0):
                raise Echec(f"[{chemin}] devait etre faux, vaut {obtenu!r}")
            if verbe in ("gt", "gte", "lt", "lte"):
                ok = {"gt": obtenu > attendu, "gte": obtenu >= attendu,
                      "lt": obtenu < attendu, "lte": obtenu <= attendu}[verbe]
                if not ok:
                    raise Echec(f"[{chemin}] {obtenu!r} pas {verbe} {attendu!r}")
            if verbe == "contains" and attendu not in (obtenu or []):
                raise Echec(f"[{chemin}] ne contient pas {attendu!r}")
            continue
        raise Saute(f"verbe [{verbe}] inconnu du runner")


def nettoie(serveur):
    """Table rase entre deux cas.

    Un test laisse parfois un index en lecture seule ou ferme : sans lever le
    blocage d'abord, la suppression echoue en 403 et **tous** les cas suivants
    tombent en cascade sur « index already exists ». C'est ce qui a fait echouer
    ce runner contre un vrai Elasticsearch avant qu'il ne serve a mesurer quoi
    que ce soit.
    """
    serveur.appelle("indices.put_settings", {
        "index": "*", "expand_wildcards": "all", "ignore_unavailable": True,
        "body": {"index.blocks.read_only": None,
                 "index.blocks.read_only_allow_delete": None,
                 "index.blocks.write": None,
                 "index.blocks.metadata": None},
    })
    statut, liste = serveur.appelle("indices.delete", {
        "index": "*", "ignore_unavailable": True, "expand_wildcards": "all"})
    if statut < 400:
        return
    # Le joker n'est pas universel — ferrite exige un index nomme. On enumere
    # alors, plutot que de laisser un index survivre : sans table rase, le cas
    # suivant echoue sur « already exists » et la mesure ne veut plus rien dire.
    _, indices = serveur.appelle("cat.indices", {"format": "json"})
    for entree in indices or []:
        nom = entree.get("index") if isinstance(entree, dict) else None
        if nom:
            serveur.appelle("indices.delete", {"index": nom, "ignore_unavailable": True})


def main():
    assure_spec()
    try:
        import yaml
    except ImportError:
        print("il manque PyYAML : pip install pyyaml", file=sys.stderr)
        return 2

    serveur = Serveur(URL)
    info = serveur.appelle("info", {})[1] or {}
    print(f"== cible : {URL} — {info.get('version', {}).get('number', '?')}")
    print(f"== suite REST d'Elasticsearch {VERSION} (Apache 2.0), "
          f"{len(SUITES)} domaines\n")

    total = reussis = refuses = sautes = 0
    echecs = []
    par_suite = {}
    for suite in SUITES:
        dossier = os.path.join(SPEC_DIR, "test", suite)
        if not os.path.isdir(dossier):
            continue
        compte = [0, 0, 0, 0]  # reussis, refuses, sautes, echecs
        for fichier in sorted(os.listdir(dossier)):
            if not fichier.endswith(".yml"):
                continue
            if "_with_types" in fichier:
                # L'API typee (`/{index}/{type}/{id}`) a disparu en 8.x : ces
                # fichiers decrivent une version d'ES que ferrite n'annonce pas.
                continue
            chemin = os.path.join(dossier, fichier)
            with open(chemin) as f:
                try:
                    docs = [d for d in yaml.safe_load_all(f) if d]
                except yaml.YAMLError as e:
                    echecs.append((f"{suite}/{fichier}", "-", f"YAML illisible : {e}"))
                    continue
            setup = teardown = []
            cas = []
            for doc in docs:
                if "setup" in doc:
                    setup = doc["setup"]
                elif "teardown" in doc:
                    teardown = doc["teardown"]
                else:
                    cas.extend(doc.items())
            for nom, actions in cas:
                total += 1
                pile = {}
                nettoie(serveur)
                try:
                    joue(serveur, setup, pile)
                    joue(serveur, actions, pile)
                    reussis += 1
                    compte[0] += 1
                except Refus as e:
                    refuses += 1
                    compte[1] += 1
                    if VERBEUX:
                        print(f"  [refus] {suite}/{fichier}: {nom}\n          {e}")
                except Saute as e:
                    sautes += 1
                    compte[2] += 1
                except (Echec, Exception) as e:  # noqa: BLE001
                    echecs.append((f"{suite}/{fichier}", nom, str(e)[:220]))
                    compte[3] += 1
                finally:
                    try:
                        joue(serveur, teardown, pile)
                    except Exception:  # noqa: BLE001
                        pass
        par_suite[suite] = compte

    print(f"  {'suite':<24} {'reussis':>8} {'refuses':>8} {'sautes':>7} {'echecs':>7}")
    for suite, c in par_suite.items():
        if sum(c):
            print(f"  {suite:<24} {c[0]:>8} {c[1]:>8} {c[2]:>7} {c[3]:>7}")
    print(f"  {'TOTAL':<24} {reussis:>8} {refuses:>8} {sautes:>7} {len(echecs):>7}"
          f"   sur {total} cas")

    if echecs:
        print(f"\n== {len(echecs)} echecs — ferrite repond, mais autre chose qu'ES")
        for fichier, nom, detail in echecs[: (10_000 if VERBEUX else 40)]:
            print(f"  {fichier}: {nom}\n      {detail}")
        if not VERBEUX and len(echecs) > 40:
            print(f"  ... et {len(echecs) - 40} autres (--verbeux pour tout voir)")
    nettoie(serveur)
    return 1 if echecs else 0


if __name__ == "__main__":
    sys.exit(main())
