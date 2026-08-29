#!/usr/bin/env python3
"""Faire passer a ferrite la suite de conformance **d'un autre moteur**.

    python3 tests/compat/conformance_es.py [URL] [--suites search,get,...] [--verbeux]
    python3 tests/compat/conformance_es.py [URL] --json docs/conformance.json
    python3 tests/compat/conformance_es.py [URL] --diff docs/conformance.json
    python3 tests/compat/conformance_es.py [URL] --etat
    python3 tests/compat/conformance_es.py [URL] --source opensearch \\
        --divergences docs/conformance-opensearch-es8150.json

Le harnais de ce repo compare ferrite a un vrai ES sur des cas qu'on a ecrits.
Celui-ci change de nature : les cas viennent d'**ailleurs**, pas de nous. C'est
la seule facon d'attraper ce qu'on ne sait pas qu'on ignore — un test qu'on
ecrit soi-meme porte la meme idee fausse que le code qu'il teste.

# D'ou viennent ces tests, et pourquoi on a le droit

Elasticsearch publie ses tests REST au format YAML : une suite d'appels et
d'assertions, concue pour etre rejouee par n'importe quel client contre
n'importe quel serveur. **La version 7.10.2 est la derniere publiee sous licence
Apache 2.0** (la 7.11 bascule en SSPL + Elastic License) : c'est donc celle-la
qu'on utilise, et elle est compatible avec la licence de ferrite. Heureuse
coincidence, c'est aussi la version du projet qu'on cherche a servir.

# Deux sources, et pourquoi la seconde

Une seule suite, c'est un examen dont on connait le sujet. Celle d'Elastic a en
plus deux limites qu'on ne peut pas lui retirer : elle est **figee en 2020**, et
une partie de ses echecs porte sur ce que la 8.x a supprime (`include_type_name`,
`_type` dans les reponses, `action.destructive_requires_name` a `false`).

`--source opensearch` joue la suite REST d'**OpenSearch**
(`opensearch-project/OpenSearch`), qui descend du meme fork de 2020 mais a ete
maintenue et etendue depuis. Le format des cas est le meme, donc c'est le meme
runner qui la joue — le tri se fait dans le rapport, pas dans un second chemin.
Deux equipes differentes qui trouvent le meme trou, c'est une mesure.

**Licence, verifiee avant de s'en servir** : OpenSearch est publie sous
**Apache-2.0** (`LICENSE.txt` a la racine du depot, « Apache License, Version
2.0 »), comme la 7.10.2 d'Elastic. Les deux sont compatibles avec la licence de
ferrite.

Les fichiers ne sont pas recopies dans ce depot : ils sont telecharges a la
demande dans `.es-rest-spec/` et `.opensearch-rest-spec/` (ignores par git).
Provenance et licence restent donc chez leurs auteurs, et ce fichier n'en
contient aucune ligne.

# Ce que le runner sait faire

Le vocabulaire de ces tests est petit : `do`, `match`, `length`, `is_true`,
`is_false`, `gt/gte/lt/lte`, `set`, `skip`, plus `setup` / `teardown`. Les URL
sont resolues depuis les specs d'API (`api/*.json`) du meme depot, donc rien
n'est code en dur ici.

# Comment lire le resultat

Quatre categories, et c'est la distinction qui compte :

  reussis      ferrite repond comme le moteur d'origine de la suite
  refuses      ferrite refuse explicitement (hors perimetre) — attendu, pas un bug
  sautes       le cas ne mesure pas la cible (borne de version, verbe du runner)
  echecs       ferrite repond, mais autre chose : **ce sont les vrais**

Trois taux en sortent, qui ne repondent pas a la meme question :

  fidelite                             reussis / (reussis + echecs)
      Un pis-aller : une partie des echecs sont des refus, mais dont le type
      d'erreur imite Elasticsearch au lieu de porter le marqueur. Ils gonflent
      le denominateur alors qu'ils sont hors perimetre.
  fidelite dans le perimetre declare   reussis / (reussis + regressions + indetermines)
      Le meme, en croisant chaque echec avec `compat.yaml` : un echec sur une
      capacite declaree **refusee** sort du denominateur (c'est le cout affiche
      du perimetre), un echec sur une capacite declaree **supportee** y reste
      (c'est une regression). Un cas qu'aucune capacite ne reclame y reste
      aussi : un trou dans la declaration ne doit pas flatter le taux.
  couverture brute                     reussis / total
      Quelle part de la suite passe, perimetre non declare compris.

Le premier dit « est-ce que ce qu'on annonce est juste », le dernier « quelle
part du moteur d'origine on couvre ». Confondre les deux, c'est soit se flatter,
soit se punir. Le rattachement est explique dans `perimetre.py` ; il est mesure
par ce que le serveur repond, pas decide ici.

# La troisieme categorie : les deux moteurs ne s'accordent pas

Un cas de la suite d'OpenSearch qui echoue contre ferrite n'echoue pas
forcement **parce que** c'est ferrite : ferrite reproduit Elasticsearch 8.15, et
OpenSearch a diverge d'Elasticsearch depuis 2021. Un cas qui exerce ce sur quoi
les deux moteurs ne sont plus d'accord ne peut rien dire de ferrite.

Cette categorie ne se decrete pas, sinon elle serait une opinion dont on
choisirait le contenu — exactement le defaut du denominateur qu'on ecrit
soi-meme. Elle se **mesure** : on joue la meme suite contre un **vrai
Elasticsearch 8.15**, et un cas que ce vrai Elasticsearch echoue lui aussi est
range `divergence_moteurs`. C'est ce que fait `--divergences <rapport.json>`, ou
le rapport est celui de la meme suite contre un vrai moteur.

Trois garde-fous, parce qu'un rapport de reference mal choisi rendrait la
categorie plus flatteuse sans rien dire :

  - la reference doit porter la **meme suite** (meme source, meme version), sinon
    les identifiants de cas ne designent pas les memes cas ;
  - la reference ne doit pas etre ferrite : un rapport de ferrite contre
    lui-meme classerait tous ses echecs en « divergence » ;
  - les cas que la reference **ne couvre pas** sont comptes et imprimes. Une
    categorie qui se tait sur ce qu'elle n'a pas mesure se lit « rien a
    signaler ».

Le rapport compte aussi les cas que ferrite **reussit** la ou la reference
echoue : c'est le sens qui flatte, donc celui qu'il faut regarder en premier.

# Le rapport machine

`--json <fichier>` ecrit le rapport complet (metadonnees de la mesure, totaux,
detail par suite et par cas). C'est lui la source : `docs/conformance.md` le
cite plutot que de recopier des chiffres a la main, et la CI le compare.

`--diff <ancien.json>` rejoue la suite puis dit ce qui a bouge depuis ce
rapport : les cas passes d'echec a reussi, et l'inverse. Avec `--diff`, le code
de sortie devient un **cliquet** : 0 tant que le nombre d'echecs n'augmente pas
et qu'aucun cas ne regresse de reussi a echec, 1 sinon. Sans `--diff`, il vaut
1 des qu'il reste un echec (le comportement d'origine).

Le runner lui-meme se verifie en le lancant contre un vrai Elasticsearch 7.10.2 :
il doit y etre pratiquement tout vert. Un runner qui echoue partout contre ES ne
prouve rien sur ferrite.

# L'etat entre deux cas se verifie, il ne se suppose pas

Un cas qui laisse derriere lui un index, un alias, un template ou un reglage de
cluster fait echouer les suivants pour une raison qui ne leur appartient pas —
et le rapport devient faux sans rien signaler. `--etat` releve huit sortes
d'etat entre chaque paire de cas et **arrete la campagne** au premier ecart,
plutot que de laisser le cas suivant en heriter.

La reference n'est pas le vide : un vrai Elasticsearch demarre avec ses propres
templates et les **reinstalle** apres que `nettoie` les a supprimes. C'est donc
l'etat de depart de la cible, releve avant le premier nettoyage, et seules les
**apparitions** par rapport a lui comptent (voir `etat_de_depart`).

Les sondes que la cible ne sait pas servir sont imprimees au demarrage : un mode
qui repondrait « etat propre » sans avoir pose la question serait exactement le
defaut qu'il corrige. Cout mesure : 3,3 s sur 12 s, soit +27 % — c'est la CI qui
le paye, a chaque passage du cliquet.
"""
import datetime
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import genere_compat  # noqa: E402
import perimetre as perimetre_declare  # noqa: E402

RACINE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..")

# Les deux suites REST qu'on sait jouer. Elles ont le meme format de cas (elles
# descendent du meme fork de 2020), donc le meme runner les joue : ce qui change
# tient dans ce tableau.
#
# `licence` n'est pas decoratif — c'est la condition pour s'en servir, et elle a
# ete verifiee dans le depot avant d'ecrire la ligne (`LICENSE.txt` a la racine
# des deux). `version_evaluee` est la version que les bornes `skip: {version}` de
# la suite decrivent : ce sont ses cas, donc c'est son numerotation.
SOURCES = {
    "elasticsearch": {
        "nom": "Elasticsearch",
        "version": "7.10.2",
        "tarball": "https://github.com/elastic/elasticsearch/archive/refs/tags/v7.10.2.tar.gz",
        "prefixe": "elasticsearch-7.10.2/rest-api-spec/src/main/resources/rest-api-spec/",
        "cache": ".es-rest-spec",
        "licence": "Apache-2.0",
        "pourquoi": "la 7.10.2 est la derniere version d'Elasticsearch publiee "
                    "sous Apache 2.0 : la 7.11 bascule en SSPL + Elastic License",
        "version_evaluee": (7, 10, 2),
    },
    "opensearch": {
        "nom": "OpenSearch",
        "version": "2.19.1",
        "tarball": "https://github.com/opensearch-project/OpenSearch/archive/refs/tags/2.19.1.tar.gz",
        "prefixe": "OpenSearch-2.19.1/rest-api-spec/src/main/resources/rest-api-spec/",
        "cache": ".opensearch-rest-spec",
        "licence": "Apache-2.0",
        "pourquoi": "OpenSearch est publie sous Apache 2.0 (LICENSE.txt a la "
                    "racine du depot) — descendant du fork de 2020, mais "
                    "maintenu et etendu depuis",
        "version_evaluee": (2, 19, 1),
    },
}

# Les options a valeur consomment l'argument qui suit : sans ca, le chemin passe
# a `--json` serait pris pour l'URL de la cible, et la mesure viserait un
# serveur qui n'existe pas — en silence.
A_VALEUR = {"--suites", "--json", "--diff", "--source", "--divergences"}


def lis_arguments(argv):
    positionnels, options = [], {}
    reste = list(argv)
    while reste:
        a = reste.pop(0)
        if a in A_VALEUR:
            if not reste:
                print(f"il manque la valeur de [{a}]", file=sys.stderr)
                sys.exit(2)
            options[a] = reste.pop(0)
        elif a.startswith("--"):
            options[a] = True
        else:
            positionnels.append(a)
    return positionnels, options


POSITIONNELS, OPTIONS = lis_arguments(sys.argv[1:])
URL = POSITIONNELS[0] if POSITIONNELS else "http://localhost:9200"
VERBEUX = "--verbeux" in OPTIONS
SORTIE_JSON = OPTIONS.get("--json")
RAPPORT_ANCIEN = OPTIONS.get("--diff")
RAPPORT_DIVERGENCES = OPTIONS.get("--divergences")

SOURCE_ID = OPTIONS.get("--source", "elasticsearch")
if SOURCE_ID not in SOURCES:
    print(f"source inconnue [{SOURCE_ID}] : {', '.join(sorted(SOURCES))}",
          file=sys.stderr)
    sys.exit(2)
SOURCE = SOURCES[SOURCE_ID]
SPEC_DIR = os.path.abspath(os.path.join(RACINE, SOURCE["cache"]))
VERSION = SOURCE["version"]
TARBALL = SOURCE["tarball"]
# Renseignes une fois la suite recuperee : on ne choisit pas les domaines, on
# les lit sur le disque (voir `suites_disponibles`).
SUITES = []
# Un sous-ensemble de suites ne mesure pas la meme chose que la suite entiere :
# le rapport le dit, et le cliquet refuse de trancher sur une mesure partielle.
PARTIEL = False
# Verifie entre deux cas que rien n'est apparu depuis l'etat de depart de la
# cible, et s'arrete au premier ecart plutot que de laisser le cas suivant en
# heriter.
ETAT_VERIFIE = "--etat" in OPTIONS

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

    print(f"== telechargement de la suite REST de {SOURCE['nom']} {VERSION} "
          f"({SOURCE['licence']}) -> {SPEC_DIR}")
    with urllib.request.urlopen(TARBALL, timeout=600) as r:
        brut = r.read()
    prefixe = SOURCE["prefixe"]
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


def suites_disponibles():
    """Tous les domaines de la suite, lus sur le disque.

    Il n'y a **pas** de liste blanche ici, et c'est le point : choisir les
    domaines qu'on joue, c'est choisir son denominateur. Un domaine entierement
    hors perimetre (snapshots, ILM, scripts) doit apparaitre dans le rapport
    avec ses cas ranges en « refuse » — visible plutot qu'absent. Le tri se fait
    dans le rapport, a partir de ce que le serveur repond, pas par omission.
    """
    racine = os.path.join(SPEC_DIR, "test")
    return sorted(n for n in os.listdir(racine)
                  if os.path.isdir(os.path.join(racine, n)))


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
        # Vrai quand le dernier appel a resolu une URL de l'API typee.
        self.derniere_url_typee = False
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
        # L'API typee (`/{index}/{type}/{id}`) a disparu en 8.x : un cas qui la
        # demande ne mesure pas la version que ferrite annonce. Ca se lit sur le
        # chemin choisi, pas sur le message d'erreur — « no handler found for
        # uri [/logs-1/test/1] » ne se distingue pas d'une route manquante.
        return url, methode, reste, "{type}" in choisi["path"]

    def appelle(self, api, params):
        params = dict(params)
        corps = params.pop("body", None)
        # `ignore` appartient au runner officiel (« ne leve pas sur ce statut »),
        # pas a l'API : il ne doit pas partir en query string.
        tolere = params.pop("ignore", None)
        tolere = [tolere] if isinstance(tolere, int) else (tolere or [])
        url, methode, qs, typee = self.url_de(api, params)
        self.derniere_url_typee = typee
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

# Les seules API qui posent un etat vivant **hors** d'un index — donc que la
# suppression des index ne defait pas. La liste est exhaustive par construction :
# un template, un pipeline, un depot ou un reglage de cluster ne peut naitre que
# la. Voir `nettoie`.
APIS_A_ETAT_GLOBAL = {
    "indices.put_template", "indices.put_index_template",
    "cluster.put_component_template", "ingest.put_pipeline",
    "snapshot.create_repository", "cluster.put_settings",
}
# Vrai des qu'un cas a touche l'une d'elles : le nettoyage complet attend ce
# signal plutot que de balayer a chaque cas ce qui n'existe pas.
ETAT_GLOBAL_SALE = True

# Ce que la cible portait **avant** la campagne, releve une fois au demarrage
# (voir `etat_de_depart`). Le nettoyage s'en sert pour ne defaire que ce que les
# cas ont pose : un vrai Elasticsearch arrive avec ses propres templates, et les
# lui retirer serait sortir du role du runner.
ETAT_DE_DEPART = {}


# Les bornes `skip: version` d'une suite sont ecrites pour un serveur de la
# version **de cette suite** : c'est donc celle qu'on evalue, quel que soit le
# numero annonce par la cible. Et la numerotation d'OpenSearch n'est pas celle
# d'Elasticsearch — sa suite se lit contre 2.19.1, pas contre 7.10.2.
VERSION_EVALUEE = SOURCE["version_evaluee"]


def _num(v):
    morceaux = [int(x) for x in re.findall(r"\d+", v)][:3]
    while len(morceaux) < 3:
        morceaux.append(0)
    return tuple(morceaux)


# Deux bornes hors de toute version reelle : une borne absente n'est pas la
# version 0.0.0 ni la 99.99.99, c'est « pas de borne de ce cote ».
PLUS_PETIT = (-1,)
PLUS_GRAND = (9,)


def cle_de_version(v):
    """La cle de tri d'un numero de version, **dans l'ordre du moteur qui a
    ecrit la suite**.

    Pour Elasticsearch, c'est l'ordre des nombres. Pour OpenSearch, non : il a
    renumerote a 1.0.0 en repartant d'Elasticsearch 7.10, et son propre
    comparateur range les versions « legacy » 6.x et 7.x **en dessous** de
    toutes les siennes. Lire `skip: {version: " - 7.9.99"}` comme un nombre fait
    donc sauter, sur un OpenSearch 2.19, des cas qu'il joue — et lire
    `skip: {version: "7.2.0 -"}` comme un nombre fait jouer des cas qu'il saute.

    Ce n'est pas une hypothese : c'est l'etalonnage qui l'a dit. Les deux seuls
    cas « (pre 7.2.0) » de la suite echouaient contre un **vrai OpenSearch
    2.19.1**, parce qu'ils y sont sautes et qu'on les jouait.
    """
    if SOURCE_ID == "opensearch" and v[0] >= 6:
        return (0,) + v
    return (1,) + v


def dans_la_plage(plage):
    """`skip: {version: "7.0.0 - 7.9.99"}` s'applique-t-il a notre version ?"""
    if plage.strip() == "all":
        return True
    courant = cle_de_version(VERSION_EVALUEE)
    for morceau in str(plage).split(","):
        bas, _, haut = morceau.partition("-")
        bas = cle_de_version(_num(bas)) if bas.strip() else PLUS_PETIT
        haut = cle_de_version(_num(haut)) if haut.strip() else PLUS_GRAND
        if bas <= courant <= haut:
            return True
    return False


class Saute(Exception):
    """Le cas ne mesure pas la cible.

    Deux motifs, qui ne disent pas la meme chose : `version`, c'est la suite
    elle-meme qui declare le cas hors de la version evaluee ; `vocabulaire`,
    c'est **ce runner** qui ne sait pas jouer le cas. Le second est une
    exclusion de notre fait, donc a compter comme telle dans le rapport.
    """

    def __init__(self, raison, motif="vocabulaire"):
        super().__init__(raison)
        self.motif = motif


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


def joue(serveur, actions, pile, trace, corps_precedent=None):
    """Rejoue une liste d'actions.

    `trace` retient l'API du dernier `do` : sans elle, un echec ne dit pas
    **sur quoi** il porte, et le rapport ne peut pas le rattacher au perimetre
    declare (voir `perimetre.py`).
    """
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
                    raise Saute(arg.get("reason", f"skip {arg['version']}"),
                                "version")
            continue

        if verbe == "do":
            arg = dict(arg)
            trace["api"] = None
            attrape = arg.pop("catch", None)
            arg.pop("headers", None)
            arg.pop("warnings", None)
            arg.pop("allowed_warnings", None)
            arg.pop("node_selector", None)
            if not arg:
                continue
            # Un `do` porte normalement **une** API. Il arrive qu'un cas en
            # empile deux dans le meme bloc (`indices.create` puis `bulk` dans
            # `index/90_unsigned_long.yml` d'OpenSearch) : n'en jouer que la
            # premiere laissait le document non indexe, donc « 1 document au
            # lieu de 2 » — un echec qui ne ressemble pas a un defaut du runner.
            # Les jouer toutes, dans l'ordre du fichier, ne change rien au cas
            # normal.
            for api, params in list(arg.items()):
                trace["api"] = api
                trace["api_typee"] = False
                if api in APIS_A_ETAT_GLOBAL:
                    global ETAT_GLOBAL_SALE
                    ETAT_GLOBAL_SALE = True
                params = resous(params or {}, pile)
                try:
                    statut, reponse = serveur.appelle(api, params)
                    trace["api_typee"] = serveur.derniere_url_typee
                except KeyError as e:
                    # Aucun chemin ne correspond : un vrai client refuserait
                    # aussi. C'est une reponse valable quand le test attend une
                    # erreur.
                    if attrape in ("param", "request", "bad_request"):
                        break
                    raise Echec(str(e)) from e
                if isinstance(reponse, bool):  # HEAD : la reponse est le booleen
                    break
                verifie_la_reponse(api, statut, reponse, attrape)
            continue

        if verbe == "set":
            for chemin, nom in arg.items():
                # `_arbitrary_key_` est une fonctionnalite du runner officiel
                # (« prends une cle quelconque de cet objet ») que celui-ci
                # n'implemente pas. Elle se declare normalement en
                # `features: [arbitrary_key]`, et le cas est alors saute — mais
                # tous les cas ne la declarent pas. La detecter sur l'action
                # plutot que sur la declaration evite de rendre un echec la ou
                # il n'y a qu'un verbe qu'on ne sait pas jouer.
                if "_arbitrary_key_" in chemin:
                    raise Saute("exige ['arbitrary_key']")
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
            # `int(attendu)` : un cas ecrit parfois la longueur entre guillemets
            # (`length: {…: "1"}`). Comparer sans convertir rendait « longueur 1
            # != 1 », un message qui accuse le serveur d'un defaut du runner.
            if verbe == "length" and len(obtenu or []) != int(attendu):
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


def verifie_la_reponse(api, statut, reponse, attrape):
    """Ce que le serveur a rendu est-il ce que le cas attendait ?"""
    err = reponse.get("error", {}) if isinstance(reponse, dict) else {}
    err = err if isinstance(err, dict) else {}
    # Un refus de ferrite peut arriver **enveloppe** : une erreur de la phase de
    # fetch (un `docvalue_fields` sur un `text`, un `format` sur un numerique)
    # sort dans le « all shards failed » d'ES, dont le `type` est celui de
    # l'enveloppe et non celui du refus. Ne regarder que le `type` de tete
    # faisait alors passer un cout de perimetre pour une regression — le rapport
    # designait la mauvaise chose a corriger.
    racines = err.get("root_cause") or []
    types = [err.get("type")] + [
        r.get("type") for r in racines if isinstance(r, dict)]
    if REFUS_FERRITE in types:
        motif = err.get("reason") or ""
        for r in racines:
            if isinstance(r, dict) and r.get("type") == REFUS_FERRITE:
                motif = r.get("reason") or motif
                break
        raise Refus(motif[:150])
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


def nettoie(serveur):
    """Table rase entre deux cas.

    Un test laisse parfois un index en lecture seule ou ferme : sans lever le
    blocage d'abord, la suppression echoue en 403 et **tous** les cas suivants
    tombent en cascade sur « index already exists ». C'est ce qui a fait echouer
    ce runner contre un vrai Elasticsearch avant qu'il ne serve a mesurer quoi
    que ce soit.

    Un index n'est pas le seul etat qu'un cas laisse derriere lui, et le reste
    ment plus discretement. Un **template** survivant s'applique aux index que
    les cas suivants creent : `mget` lisait alors un `_type` la ou le cas
    attendait `null`, et `indices.stats` refusait d'indexer (« more than 1
    type »), pour la seule raison qu'un template pose vingt cas plus tot donnait
    un mapping `_doc` a tout index nomme `t*`. Rien dans ces echecs ne
    ressemblait a une fuite d'etat — d'ou le nettoyage complet : templates,
    pipelines, depots, et les reglages de cluster.

    Cette seconde moitie ne se paye que quand elle sert. Seules les six API
    listees dans `APIS_A_ETAT_GLOBAL` creent cet etat ; tant qu'aucune n'a ete
    appelee, il n'y a rien a balayer, et le balayer quand meme coutait plus de
    temps que la suite entiere n'en prend.
    """
    global ETAT_GLOBAL_SALE
    if not ETAT_GLOBAL_SALE:
        return nettoie_les_index(serveur)
    ETAT_GLOBAL_SALE = False
    # Un depot desenregistre garde ses fichiers : le cas suivant qui reenregistre
    # le meme chemin y retrouve les snapshots du precedent (« snapshot with the
    # same name already exists »). Les snapshots se suppriment donc avant leur
    # depot, et depot par depot — le joker n'est pas accepte sur le depot.
    try:
        _, depots = serveur.appelle("snapshot.get_repository", {})
    except (KeyError, urllib.error.URLError, OSError):
        depots = None
    for nom in (depots if isinstance(depots, dict) and "error" not in depots else {}):
        serveur.appelle("snapshot.delete", {"repository": nom, "snapshot": "*"})
    for balayage in BALAYAGES:
        balaye(serveur, *balayage)
    # `"*": null` remet a leur defaut *tous* les reglages poses : sans ca, un
    # `cluster.routing.allocation.enable: none` laisse par un cas se lit encore
    # dans le cas suivant, qui le prend pour sa propre reponse.
    serveur.appelle("cluster.put_settings", {
        "body": {"transient": {"*": None}, "persistent": {"*": None}}})
    return nettoie_les_index(serveur)


def balaye(serveur, nom, api_suppr, cle, api_liste, extrait):
    """Supprimer tout ce qu'un cas a pu poser d'une sorte d'etat global.

    Le joker d'abord, parce qu'il coute un appel. Mais un vrai Elasticsearch 8
    refuse `DELETE /_component_template/*` **en bloc** des qu'un seul element
    est protege — ses templates de composants integres sont « still in use by
    index templates » — et alors *rien* n'est supprime, pas meme ce que le cas
    vient de poser. On enumere donc, et on ne touche qu'a ce qui n'etait pas la
    au demarrage : le runner defait ce que les cas ont fait, il ne demonte pas
    le serveur qu'on lui prete. Trouve par `--etat` contre un ES 8.15, sur la
    suite d'OpenSearch.
    """
    try:
        statut, _ = serveur.appelle(api_suppr, {cle: "*"})
    except (KeyError, urllib.error.URLError, OSError):
        return
    if statut < 400:
        return
    try:
        _, liste = serveur.appelle(api_liste, {})
    except (KeyError, urllib.error.URLError, OSError):
        return
    if not isinstance(liste, (dict, list)) or (
            isinstance(liste, dict) and "error" in liste):
        return
    try:
        noms = extrait(liste)
    except (AttributeError, TypeError):
        return
    for n in sorted(noms - ETAT_DE_DEPART.get(nom, set())):
        try:
            serveur.appelle(api_suppr, {cle: n})
        except (KeyError, urllib.error.URLError, OSError):
            pass


def nettoie_les_index(serveur):
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
    # Le joker n'est pas universel — ferrite exige un index nomme, et un vrai
    # Elasticsearch 8 refuse `DELETE /*` tant que `action.destructive_requires_name`
    # vaut `true`. On enumere alors, plutot que de laisser un index survivre :
    # sans table rase, le cas suivant echoue sur « already exists » et la mesure
    # ne veut plus rien dire.
    #
    # `expand_wildcards: all` n'est pas decoratif : sans lui, l'enumeration ne
    # voit pas les index **caches**, et `cat.aliases/40_hidden.yml` laissait
    # derriere lui un `test` cache et son alias. Contre un ES 7.10 le chemin
    # n'etait jamais emprunte (le joker y passait) ; c'est le mode `--etat`,
    # contre un ES 8.15, qui l'a nomme.
    _, indices = serveur.appelle("cat.indices",
                                 {"format": "json", "expand_wildcards": "all"})
    for entree in indices or []:
        nom = entree.get("index") if isinstance(entree, dict) else None
        if nom:
            serveur.appelle("indices.delete", {"index": nom, "ignore_unavailable": True})


# ---------------------------------------------------------------------------
# L'etat entre deux cas, verifie plutot que suppose
# ---------------------------------------------------------------------------

def _noms_de_cat(reponse):
    return {e["index"] for e in (reponse or []) if isinstance(e, dict) and e.get("index")}


def _noms_d_alias(reponse):
    return {a for v in (reponse or {}).values() if isinstance(v, dict)
            for a in (v.get("aliases") or {})}


def _noms_de_liste(cle):
    return lambda reponse: {t["name"] for t in ((reponse or {}).get(cle) or [])
                            if isinstance(t, dict) and t.get("name")}


def _cles(reponse):
    return set(reponse or {})


def _cles_de_reglages(reponse):
    """Les reglages poses, `portee.cle.pointee` par `portee.cle.pointee`."""
    def plat(prefixe, valeur):
        if isinstance(valeur, dict):
            for k, v in valeur.items():
                yield from plat(f"{prefixe}.{k}", v)
        else:
            yield prefixe
    trouves = set()
    for portee in ("persistent", "transient"):
        trouves |= set(plat(portee, (reponse or {}).get(portee) or {}))
    return trouves


# Les sortes d'etat qu'un cas peut laisser derriere lui, et comment nommer ce
# qu'on y trouve. La liste ne se devine pas : les cinq premieres lignes y sont
# parce qu'un cas les a laissees au moins une fois, les trois dernieres parce
# qu'elles survivent a la suppression des index chez Elasticsearch.
SONDES_D_ETAT = (
    ("index", "cat.indices", {"format": "json", "expand_wildcards": "all"},
     _noms_de_cat),
    ("alias", "indices.get_alias", {}, _noms_d_alias),
    ("template", "indices.get_template", {}, _cles),
    ("template d'index", "indices.get_index_template", {},
     _noms_de_liste("index_templates")),
    ("reglage de cluster", "cluster.get_settings", {}, _cles_de_reglages),
    ("template de composants", "cluster.get_component_template", {},
     _noms_de_liste("component_templates")),
    ("pipeline", "ingest.get_pipeline", {}, _cles),
    ("depot de snapshots", "snapshot.get_repository", {}, _cles),
)

# Les cinq sortes d'etat global que `nettoie` balaie, et de quoi les enumerer
# quand le joker est refuse : (nom d'etat, API de suppression, nom du parametre,
# API de listage, extracteur des noms). Le nom d'etat est celui de
# `SONDES_D_ETAT` — c'est ce qui permet de ne supprimer que ce qui n'etait pas
# la au demarrage.
BALAYAGES = (
    ("template", "indices.delete_template", "name",
     "indices.get_template", _cles),
    ("template d'index", "indices.delete_index_template", "name",
     "indices.get_index_template", _noms_de_liste("index_templates")),
    ("template de composants", "cluster.delete_component_template", "name",
     "cluster.get_component_template", _noms_de_liste("component_templates")),
    ("pipeline", "ingest.delete_pipeline", "id", "ingest.get_pipeline", _cles),
    ("depot de snapshots", "snapshot.delete_repository", "repository",
     "snapshot.get_repository", _cles),
)


class FuiteDEtat(Exception):
    """Un cas a laisse derriere lui un etat que `nettoie` n'attrape pas.

    Ce n'est pas un echec de cas : c'est un defaut du runner, et il rend faux
    le verdict de **tous** les cas qui suivent. D'ou l'arret immediat.
    """


def _releve(serveur, sonde):
    """Ce que porte la cible pour cette sonde : un ensemble de noms, ou None si
    la sonde ne repond pas (route refusee, cible muette)."""
    _, api, params, extrait = sonde
    try:
        _, reponse = serveur.appelle(api, dict(params))
    except (KeyError, urllib.error.URLError, OSError):
        return None
    if isinstance(reponse, dict) and "error" in reponse:
        return None
    try:
        return extrait(reponse)
    except (AttributeError, TypeError):
        return None


def etat_de_depart(serveur):
    """Ce que la cible porte **avant** qu'on ne joue quoi que ce soit.

    La reference n'est pas « rien » : un vrai Elasticsearch demarre avec ses
    propres templates (`ilm-history`, `.transform-notifications-*`, les
    templates de composants de x-pack), et il les **reinstalle** apres que
    `nettoie` les a supprimes. Mesurer contre le vide dirait « fuite » a chaque
    cas contre la cible qui sert justement a etaler l'instrument.

    La reference est donc l'etat de depart de la cible, releve avant le premier
    nettoyage. Ce qui apparait par rapport a lui, et rien d'autre, est ce qu'un
    cas a laisse derriere lui.

    Rend `(reference, muettes)` : les sondes que la cible ne sait pas servir
    sont ecartees **et nommees** — un mode qui repondrait « etat propre » sans
    avoir pose la question serait exactement le defaut qu'il corrige.
    """
    reference, muettes = {}, []
    for sonde in SONDES_D_ETAT:
        nom = sonde[0]
        releve = _releve(serveur, sonde)
        if releve is None:
            muettes.append(nom)
            continue
        reference[nom] = releve
    return reference, muettes


def verifie_l_etat(serveur, reference):
    """Ce qui est apparu depuis l'etat de depart. Vide quand tout va bien.

    Seules les **apparitions** comptent. Une disparition ne se lit pas : contre
    un vrai Elasticsearch, `nettoie` supprime les templates de x-pack et ES les
    reinstalle a son rythme — on lirait sa reinstallation comme une fuite et son
    absence momentanee comme une autre.
    """
    fuites = []
    for sonde in SONDES_D_ETAT:
        nom = sonde[0]
        if nom not in reference:
            continue
        releve = _releve(serveur, sonde)
        if releve is None:
            # Une sonde qui repondait au demarrage et se tait maintenant a
            # change d'avis a cause de ce qu'un cas a laisse : c'est une fuite.
            fuites.append(f"{nom} : la sonde ne repond plus")
            continue
        apparus = sorted(releve - reference[nom])
        if apparus:
            fuites.append(f"{nom} : {', '.join(apparus)[:300]}")
    return fuites


# ---------------------------------------------------------------------------
# Le rapport machine
# ---------------------------------------------------------------------------

# Version du format de `docs/conformance.json`. Un lecteur qui ne connait pas
# le numero qu'il trouve doit s'arreter, pas deviner.
#
#   1  totaux, deux taux, detail par suite et par cas
#   2  chaque echec est rattache au perimetre declare (`compat.yaml`) :
#      regression / cout de perimetre / indetermine, et le troisieme taux
#   3  la suite jouee est nommee (`mesure.suite_rest.source`), et un echec peut
#      porter un quatrieme verdict, `divergence_moteurs` : la suite vient d'un
#      moteur, un vrai moteur de reference echoue le meme cas — mesure, pas
#      decidee (voir `--divergences`)
SCHEMA = 3

DIVERGENCE = "divergence_moteurs"

VERDICTS = (perimetre_declare.REGRESSION, perimetre_declare.COUT,
            perimetre_declare.INDETERMINE, DIVERGENCE)

CATEGORIES = ("reussi", "refus", "saute", "echec")
PLURIEL = {"reussi": "reussis", "refus": "refuses", "saute": "sautes",
           "echec": "echecs"}


def cle_de(cas):
    """L'identite d'un cas, stable d'un rapport a l'autre."""
    return f"{cas['suite']}/{cas['fichier']}::{cas['cas']}"


def etat_du_depot():
    """Le commit mesure. Sans lui, un rapport ne dit pas de quoi il parle."""
    def git(*a):
        try:
            r = subprocess.run(["git", "-C", RACINE, *a], capture_output=True,
                               text=True, timeout=10)
            return r.stdout.strip() if r.returncode == 0 else ""
        except (OSError, subprocess.SubprocessError):
            return ""
    # `-uno` : un fichier non suivi (le venv, un rapport de travail) ne rend pas
    # la mesure douteuse ; une modification non commitee, si.
    return git("rev-parse", "HEAD") or None, bool(git("status", "--porcelain", "-uno"))


def taux_de(totaux, perimetre_totaux=None):
    """Les taux, avec de quoi les recalculer — un taux sans son denominateur
    n'est pas verifiable."""
    perimetre = totaux["reussis"] + totaux["echecs"]
    taux = {
        "fidelite_perimetre": {
            "valeur": round(totaux["reussis"] / perimetre, 4) if perimetre else None,
            "numerateur": totaux["reussis"],
            "denominateur": perimetre,
            "definition": "reussis / (reussis + echecs) — la part des cas que "
                          "ferrite n'a ni refuses ni fait sauter qui repond "
                          "comme Elasticsearch. Un pis-aller : une partie des "
                          "echecs sont des refus dont le type d'erreur imite "
                          "celui d'ES au lieu de porter le marqueur, donc ils "
                          "gonflent le denominateur alors qu'ils sont hors "
                          "perimetre. Voir [fidelite_perimetre_declare]",
        },
        "couverture_brute": {
            "valeur": round(totaux["reussis"] / totaux["cas"], 4) if totaux["cas"] else None,
            "numerateur": totaux["reussis"],
            "denominateur": totaux["cas"],
            "definition": "reussis / total — la part de la suite d'Elastic qui "
                          "passe, perimetre non declare compris",
        },
    }
    if perimetre_totaux:
        # Le denominateur ne retire que ce qui est **declare** refuse dans
        # compat.yaml. Les indetermines y restent : un trou dans la declaration
        # ne doit pas flatter le taux.
        dedans = (totaux["reussis"] + perimetre_totaux["regressions"]
                  + perimetre_totaux["indetermines"])
        taux["fidelite_perimetre_declare"] = {
            "valeur": round(totaux["reussis"] / dedans, 4) if dedans else None,
            "numerateur": totaux["reussis"],
            "denominateur": dedans,
            "definition": "reussis / (reussis + regressions + indetermines) — "
                          "le taux precedent, mais en sortant du denominateur "
                          "les echecs qui portent sur une capacite declaree "
                          "refusee dans compat.yaml (le cout du perimetre), et "
                          "ceux qu'un vrai moteur de reference echoue lui aussi "
                          "(divergence_moteurs, mesuree — voir [divergences]). "
                          "Les cas qu'aucune capacite ne reclame restent dedans",
        }
    return taux


def exclusions_de(cas, with_types):
    """Ce que le denominateur laisse dehors, et pourquoi.

    Le denominateur (`totaux.cas`) porte tous les cas que ce runner sait jouer.
    Restent deux exclusions, comptees ici plutot que passees sous silence : les
    fichiers de l'API typee, jamais ouverts, et les cas que le vocabulaire du
    runner ne sait pas jouer, ouverts mais non mesurables. La troisieme ligne
    n'est pas une exclusion de notre fait — c'est la suite qui borne ses cas par
    version — mais elle acheve de decomposer la colonne « sautes ».
    """
    def sautes(motif):
        return sum(1 for c in cas
                   if c["categorie"] == "saute" and c.get("motif") == motif)

    return {
        "fichiers_with_types": dict(with_types, pourquoi=
            "l'API typee (/{index}/{type}/{id}) a disparu en 8.x : ces fichiers "
            "decrivent une version d'ES que ferrite n'annonce pas. Jamais joues, "
            "donc hors du denominateur"),
        "cas_hors_vocabulaire": {
            "cas": sautes("vocabulaire"),
            "pourquoi": "le cas exige un verbe ou une feature que ce runner "
                        "n'implemente pas. Comptes en « sautes »",
        },
        "cas_hors_version": {
            "cas": sautes("version"),
            "pourquoi": "la suite elle-meme borne le cas hors de la version "
                        f"evaluee ({'.'.join(str(x) for x in VERSION_EVALUEE)}). "
                        "Comptes en « sautes »",
        },
    }


def croise_avec_le_perimetre(cas, reference=None):
    """Range chaque echec : divergence, regression, cout de perimetre, indetermine.

    C'est ce qui separe un chiffre qu'on subit d'un chiffre qu'on pilote. Un
    echec sur `_snapshot` (declare hors perimetre) et un echec sur `_search`
    (declare supporte) pesaient jusqu'ici pareil dans le taux de fidelite.

    Un cas qu'aucune capacite ne reclame est compte `indetermine`, **contre**
    nous : sinon, oublier de declarer une capacite ferait monter le taux.

    `reference` est le meme rapport, joue contre un **vrai** moteur (voir
    `--divergences`). Un echec que ce moteur echoue lui aussi ne mesure pas
    ferrite : il mesure un desaccord entre le moteur d'origine de la suite et
    celui que ferrite reproduit. Ce verdict passe **avant** les trois autres,
    parce qu'il porte sur le pouvoir discriminant du cas, pas sur ce qu'on
    declare : un cas qu'aucun Elasticsearch ne passe ne peut pas etablir une
    regression de ferrite, meme sur une capacite declaree supportee.

    La raison de l'echec de la reference est **conservee** a cote de celle de
    ferrite : les deux moteurs peuvent buter au meme endroit pour deux raisons
    differentes, et c'est au lecteur du rapport de le voir plutot qu'au runner
    de le cacher.
    """
    try:
        index = perimetre_declare.Perimetre()
    except (OSError, genere_compat.Invalide) as e:
        print(f"compat.yaml illisible, le croisement est saute : {e}", file=sys.stderr)
        return None
    reference = reference or {}
    compte = {v: 0 for v in VERDICTS}
    par_capacite = {}
    non_couverts = 0
    for c in cas:
        if c["categorie"] != "echec":
            continue
        ref = reference.get(cle_de(c))
        if reference and ref is None:
            non_couverts += 1
        if ref is not None and ref["categorie"] == "echec":
            c["perimetre"] = DIVERGENCE
            c["reference"] = ref
            compte[DIVERGENCE] += 1
            continue
        marqueurs = ["api_typee"] if c.get("api_typee") else []
        verdict, cid, _ = index.verdict(c.get("api"), c.get("raison"), marqueurs)
        c["perimetre"] = verdict
        if cid:
            c["capacite"] = cid
        if ref is not None:
            c["reference"] = ref
        compte[verdict] += 1
        cle = cid or "(non declaree)"
        par_capacite[cle] = par_capacite.get(cle, 0) + 1
    return {
        "regressions": compte[perimetre_declare.REGRESSION],
        "couts_perimetre": compte[perimetre_declare.COUT],
        "indetermines": compte[perimetre_declare.INDETERMINE],
        "divergences_moteurs": compte[DIVERGENCE],
        "echecs_hors_reference": non_couverts,
        "par_capacite": dict(sorted(par_capacite.items(),
                                    key=lambda kv: (-kv[1], kv[0]))),
        "source": "compat.yaml",
    }


def moteur_de(info):
    """Quel moteur repond a cette URL — mesure sur `GET /`, pas suppose.

    Les trois se distinguent sans ambiguite : OpenSearch annonce sa
    `distribution`, ferrite signe son `build_hash`. Sans ce champ, un rapport de
    ferrite contre lui-meme pourrait servir de reference aux divergences, et
    **tous** ses echecs y deviendraient des desaccords entre moteurs.
    """
    v = (info.get("version") or {}) if isinstance(info, dict) else {}
    if str(v.get("distribution") or "").lower() == "opensearch":
        return "opensearch"
    if str(v.get("build_hash") or "").startswith("ferrite-"):
        return "ferrite"
    return "elasticsearch"


def construis_rapport(cas, info, with_types, divergences=None):
    totaux = {"cas": len(cas)}
    for categorie in CATEGORIES:
        totaux[PLURIEL[categorie]] = sum(1 for c in cas if c["categorie"] == categorie)
    par_suite = {}
    for c in cas:
        compte = par_suite.setdefault(
            c["suite"], {PLURIEL[k]: 0 for k in CATEGORIES} | {"cas": 0})
        compte[PLURIEL[c["categorie"]]] += 1
        compte["cas"] += 1
    perimetre = croise_avec_le_perimetre(
        cas, divergences["index"] if divergences else None)
    sha, sale = etat_du_depot()
    version_cible = (info.get("version") or {}) if isinstance(info, dict) else {}
    rapport = {
        "schema": SCHEMA,
        "mesure": {
            "date": datetime.datetime.now(datetime.timezone.utc)
                    .strftime("%Y-%m-%dT%H:%M:%SZ"),
            "ferrite_sha": sha,
            "ferrite_arbre_modifie": sale,
            "cible": {
                "url": URL,
                "moteur": moteur_de(info),
                "version_annoncee": version_cible.get("number"),
                "build_hash": version_cible.get("build_hash"),
            },
            "suite_rest": {
                "source": SOURCE_ID,
                "moteur": SOURCE["nom"],
                "version": VERSION,
                "licence": SOURCE["licence"],
                "pourquoi_cette_version": SOURCE["pourquoi"],
                "archive": TARBALL,
            },
            "suites": list(SUITES),
            "partiel": PARTIEL,
            "exclusions": exclusions_de(cas, with_types),
        },
        "totaux": totaux,
        "taux": taux_de(totaux, perimetre),
        "perimetre": perimetre,
        "par_suite": dict(sorted(par_suite.items())),
        "cas": sorted(cas, key=lambda c: (c["suite"], c["fichier"], c["cas"])),
    }
    if divergences:
        rapport["divergences"] = bilan_des_divergences(divergences, cas, perimetre)
    return rapport


def ecris_rapport(rapport, chemin):
    with open(chemin, "w") as f:
        json.dump(rapport, f, indent=2, ensure_ascii=False, sort_keys=False)
        f.write("\n")
    print(f"\n== rapport machine ecrit : {chemin}")


def lis_rapport(chemin):
    with open(chemin) as f:
        ancien = json.load(f)
    if ancien.get("schema") != SCHEMA:
        raise ValueError(f"schema {ancien.get('schema')} inconnu "
                         f"(ce runner ecrit le {SCHEMA})")
    return ancien


def lis_les_divergences(chemin):
    """Le rapport de la **meme suite** contre un **vrai** moteur.

    Trois refus plutot que trois hypotheses, parce que chacun rendrait la
    troisieme categorie plus grosse sans rien mesurer de plus : une suite
    differente (les identifiants de cas ne designeraient pas les memes cas), une
    mesure partielle (les cas absents passeraient pour non divergents), et
    surtout une reference qui serait ferrite lui-meme.
    """
    rapport = lis_rapport(chemin)
    m = rapport["mesure"]
    suite = m.get("suite_rest") or {}
    if suite.get("source") != SOURCE_ID or suite.get("version") != VERSION:
        raise ValueError(
            f"porte la suite {suite.get('source')} {suite.get('version')}, "
            f"pas {SOURCE_ID} {VERSION} : les cas ne se correspondent pas")
    if m.get("partiel"):
        raise ValueError("mesure partielle : les cas qu'elle n'a pas joues "
                         "passeraient pour non divergents")
    moteur = (m.get("cible") or {}).get("moteur")
    if moteur == "ferrite":
        raise ValueError("la reference est ferrite : tous ses echecs "
                         "deviendraient des desaccords entre moteurs")
    if moteur is None:
        raise ValueError("la reference ne dit pas quel moteur elle a mesure")
    return {
        "chemin": chemin,
        "mesure": m,
        "index": {cle_de(c): {"categorie": c["categorie"],
                              "raison": c.get("raison", "")}
                  for c in rapport["cas"]},
    }


def bilan_des_divergences(divergences, cas, perimetre):
    """Ce que la reference a servi a trancher, et ce qu'elle n'a pas couvert.

    Le compte qui flatte est imprime le premier : les cas que ferrite **reussit**
    la ou la reference echoue. Un resultat massivement vert n'est pas plus
    fiable qu'un rouge, et c'est celui-la qui n'alarme personne.
    """
    index = divergences["index"]
    m = divergences["mesure"]
    reussis_ou_la_reference_echoue = sorted(
        cle_de(c) for c in cas if c["categorie"] == "reussi"
        and (index.get(cle_de(c)) or {}).get("categorie") == "echec")
    absents = sorted(cle_de(c) for c in cas if cle_de(c) not in index)
    return {
        "rapport": divergences["chemin"],
        "reference": {
            "moteur": (m.get("cible") or {}).get("moteur"),
            "version_annoncee": (m.get("cible") or {}).get("version_annoncee"),
            "url": (m.get("cible") or {}).get("url"),
            "date": m.get("date"),
        },
        "definition": "un echec que la reference — un vrai moteur, joue sur la "
                      "meme suite — echoue lui aussi. Le cas ne discrimine pas : "
                      "il porte sur ce dont les deux moteurs ne conviennent "
                      "plus, donc il ne peut rien dire de ferrite. Mesure, pas "
                      "decidee",
        "echecs_partages": (perimetre or {}).get("divergences_moteurs"),
        "cas_absents_de_la_reference": {
            "cas": len(absents),
            "exemples": absents[:20],
            "pourquoi": "la reference n'a pas joue ces cas : leur verdict ne "
                        "peut pas etre tranche par elle. Comptes plutot que "
                        "passes sous silence",
        },
        "reussis_alors_que_la_reference_echoue": {
            "cas": len(reussis_ou_la_reference_echoue),
            "exemples": reussis_ou_la_reference_echoue[:20],
            "pourquoi": "ferrite passe un cas qu'un vrai moteur ne passe pas. "
                        "C'est le sens qui flatte, donc celui qu'il faut lire "
                        "en premier : un defaut d'outillage s'y cache mieux "
                        "que dans un echec",
        },
    }


def compare_rapports(ancien, nouveau):
    """Ce qui a bouge entre deux rapports, cas par cas."""
    av = {cle_de(c): c for c in ancien["cas"]}
    ap = {cle_de(c): c for c in nouveau["cas"]}
    mouvements = {}
    for cle in sorted(set(av) & set(ap)):
        a, b = av[cle]["categorie"], ap[cle]["categorie"]
        if a != b:
            mouvements.setdefault(f"{a} -> {b}", []).append((cle, ap[cle].get("raison", "")))
    return {
        "mouvements": mouvements,
        "corriges": mouvements.get("echec -> reussi", []),
        "regressions": mouvements.get("reussi -> echec", []),
        "apparus": sorted(set(ap) - set(av)),
        "disparus": sorted(set(av) - set(ap)),
        "echecs_avant": ancien["totaux"]["echecs"],
        "echecs_apres": nouveau["totaux"]["echecs"],
    }


def affiche_diff(ancien, chemin_ancien, diff):
    d = ancien["mesure"]
    print(f"\n== diff contre {chemin_ancien} "
          f"(mesure du {d.get('date')}, ferrite {(d.get('ferrite_sha') or '?')[:12]})")
    delta = diff["echecs_apres"] - diff["echecs_avant"]
    print(f"   echecs : {diff['echecs_avant']} -> {diff['echecs_apres']} "
          f"({delta:+d})")
    for titre, liste in (("corriges (echec -> reussi)", diff["corriges"]),
                         ("regressions (reussi -> echec)", diff["regressions"])):
        print(f"   {titre} : {len(liste)}")
        for cle, raison in liste[: (10_000 if VERBEUX else 20)]:
            print(f"     {cle}" + (f"\n         {raison}" if raison else ""))
        if not VERBEUX and len(liste) > 20:
            print(f"     ... et {len(liste) - 20} autres (--verbeux pour tout voir)")
    autres = {k: len(v) for k, v in sorted(diff["mouvements"].items())
              if k not in ("echec -> reussi", "reussi -> echec")}
    if autres:
        print("   autres mouvements : "
              + ", ".join(f"{k} {n}" for k, n in autres.items()))
    if diff["apparus"] or diff["disparus"]:
        print(f"   cas apparus : {len(diff['apparus'])}, "
              f"disparus : {len(diff['disparus'])}")


def repond(serveur):
    """La reponse de `GET /` de la cible, ou None si elle ne repond pas."""
    try:
        return serveur.appelle("info", {})[1] or {}
    except (urllib.error.URLError, OSError):
        return None


def compte_les_cas(chemin, yaml):
    """Combien de cas porte un fichier qu'on ne joue pas.

    Une exclusion sans son compte n'est pas verifiable : c'est la seule facon
    de dire ce que le denominateur laisse dehors.
    """
    try:
        with open(chemin) as f:
            docs = [d for d in yaml.safe_load_all(f) if d]
    except (OSError, yaml.YAMLError):
        return 0
    return sum(len(d) for d in docs if "setup" not in d and "teardown" not in d)


def joue_la_suite(serveur, yaml, reference=None):
    """Rejoue toutes les suites retenues et rend un enregistrement par cas."""
    resultats = []
    with_types = {"fichiers": 0, "cas": 0}
    precedent = "(demarrage)"

    def note(suite, fichier, nom, categorie, raison="", motif=None, trace=None):
        entree = {"suite": suite, "fichier": fichier, "cas": nom,
                  "categorie": categorie, "raison": raison[:220]}
        if motif:
            entree["motif"] = motif
        # L'API du `do` qui a echoue, et si c'etait dans la mise en place : sans
        # ces deux-la, un echec ne se rattache a rien (voir `perimetre.py`), et
        # « l'echec n'est pas sur ce que le cas mesure » reste invisible.
        if categorie == "echec" and trace:
            if trace.get("api"):
                entree["api"] = trace["api"]
            if trace.get("phase") == "mise en place":
                entree["mise_en_place"] = True
            if trace.get("api_typee"):
                entree["api_typee"] = True
        resultats.append(entree)

    for suite in SUITES:
        dossier = os.path.join(SPEC_DIR, "test", suite)
        if not os.path.isdir(dossier):
            continue
        for fichier in sorted(os.listdir(dossier)):
            if not fichier.endswith(".yml"):
                continue
            chemin = os.path.join(dossier, fichier)
            if "_with_types" in fichier:
                # L'API typee (`/{index}/{type}/{id}`) a disparu en 8.x : ces
                # fichiers decrivent une version d'ES que ferrite n'annonce pas.
                with_types["fichiers"] += 1
                with_types["cas"] += compte_les_cas(chemin, yaml)
                continue
            with open(chemin) as f:
                try:
                    docs = [d for d in yaml.safe_load_all(f) if d]
                except yaml.YAMLError as e:
                    note(suite, fichier, "-", "echec", f"YAML illisible : {e}")
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
                pile = {}
                nettoie(serveur)
                if reference is not None:
                    fuites = verifie_l_etat(serveur, reference)
                    if fuites:
                        raise FuiteDEtat(
                            f"apres [{precedent}], avant "
                            f"[{suite}/{fichier}::{nom}] :\n     "
                            + "\n     ".join(fuites))
                precedent = f"{suite}/{fichier}::{nom}"
                trace = {"api": None, "phase": "mise en place"}
                try:
                    joue(serveur, setup, pile, trace)
                    trace["phase"] = "cas"
                    joue(serveur, actions, pile, trace)
                    note(suite, fichier, nom, "reussi")
                except Refus as e:
                    note(suite, fichier, nom, "refus", str(e))
                    if VERBEUX:
                        print(f"  [refus] {suite}/{fichier}: {nom}\n          {e}")
                except Saute as e:
                    note(suite, fichier, nom, "saute", str(e), e.motif)
                except (Echec, Exception) as e:  # noqa: BLE001
                    note(suite, fichier, nom, "echec", str(e), trace=trace)
                finally:
                    try:
                        joue(serveur, teardown, pile, {"api": None})
                    except Exception:  # noqa: BLE001
                        pass
    return resultats, with_types


def affiche(rapport):
    print(f"  {'suite':<24} {'reussis':>8} {'refuses':>8} {'sautes':>7} {'echecs':>7}")
    for suite, c in rapport["par_suite"].items():
        if c["cas"]:
            print(f"  {suite:<24} {c['reussis']:>8} {c['refuses']:>8} "
                  f"{c['sautes']:>7} {c['echecs']:>7}")
    t = rapport["totaux"]
    print(f"  {'TOTAL':<24} {t['reussis']:>8} {t['refuses']:>8} {t['sautes']:>7} "
          f"{t['echecs']:>7}   sur {t['cas']} cas")

    def pourcent(taux):
        v = taux["valeur"]
        return (f"{v * 100:.1f}% ({taux['numerateur']}/{taux['denominateur']})"
                if v is not None else "n/a")

    taux = rapport["taux"]
    print(f"\n  fidelite (reussis / reussis+echecs) : {pourcent(taux['fidelite_perimetre'])}")
    if "fidelite_perimetre_declare" in taux:
        print(f"  fidelite dans le perimetre declare : "
              f"{pourcent(taux['fidelite_perimetre_declare'])}")
    print(f"  couverture brute                   : {pourcent(taux['couverture_brute'])}")

    p = rapport.get("perimetre")
    d = rapport.get("divergences")
    if p:
        print(f"\n  les {rapport['totaux']['echecs']} echecs, croises avec compat.yaml :")
        if d:
            ref = d["reference"]
            print(f"    divergences      {p['divergences_moteurs']:>4}  "
                  f"{ref['moteur']} {ref['version_annoncee']} echoue le meme cas — "
                  f"ne mesure pas ferrite")
        print(f"    regressions      {p['regressions']:>4}  une capacite declaree "
              f"supportee ne repond pas comme le moteur d'origine")
        print(f"    cout de perimetre{p['couts_perimetre']:>5}  une capacite declaree "
              f"refusee — attendu")
        print(f"    indetermines     {p['indetermines']:>4}  aucune capacite ne les "
              f"reclame : a declarer dans compat.yaml")
        premiers = list(p["par_capacite"].items())[: (10_000 if VERBEUX else 12)]
        for cid, n in premiers:
            print(f"      {n:>4}  {cid}")
    if d:
        flatteurs = d["reussis_alors_que_la_reference_echoue"]
        absents = d["cas_absents_de_la_reference"]
        print(f"\n  reference des divergences : {d['rapport']}")
        print(f"    reussis alors que la reference echoue : {flatteurs['cas']}  "
              f"(le sens qui flatte)")
        for cle in flatteurs["exemples"][: (10_000 if VERBEUX else 5)]:
            print(f"      {cle}")
        print(f"    cas que la reference n'a pas joues     : {absents['cas']}")

    ex = rapport["mesure"]["exclusions"]
    print(f"\n  hors denominateur : {ex['fichiers_with_types']['cas']} cas dans "
          f"{ex['fichiers_with_types']['fichiers']} fichiers *_with_types.yml")
    print(f"  dont sautes       : {ex['cas_hors_vocabulaire']['cas']} hors "
          f"vocabulaire du runner, {ex['cas_hors_version']['cas']} bornes par "
          f"version")

    echecs = [c for c in rapport["cas"] if c["categorie"] == "echec"]
    if echecs:
        print(f"\n== {len(echecs)} echecs — ferrite repond, mais autre chose qu'ES")
        for c in echecs[: (10_000 if VERBEUX else 40)]:
            print(f"  {c['suite']}/{c['fichier']}: {c['cas']}\n      {c['raison']}")
        if not VERBEUX and len(echecs) > 40:
            print(f"  ... et {len(echecs) - 40} autres (--verbeux pour tout voir)")


def main():
    global SUITES, PARTIEL, ETAT_DE_DEPART
    assure_spec()
    try:
        import yaml
    except ImportError:
        print("il manque PyYAML : pip install pyyaml", file=sys.stderr)
        return 2

    toutes = suites_disponibles()
    SUITES = OPTIONS["--suites"].split(",") if "--suites" in OPTIONS else toutes
    PARTIEL = sorted(SUITES) != toutes

    ancien = None
    if RAPPORT_ANCIEN:
        # Lu **avant** la mesure : un rapport de reference illisible doit
        # arreter le runner tout de suite, pas apres dix minutes de suite REST.
        try:
            ancien = lis_rapport(RAPPORT_ANCIEN)
        except (OSError, ValueError, json.JSONDecodeError) as e:
            print(f"rapport de reference illisible [{RAPPORT_ANCIEN}] : {e}",
                  file=sys.stderr)
            return 2
        if PARTIEL or ancien["mesure"].get("partiel"):
            print("--diff exige deux mesures completes : une selection de suites "
                  "ne se compare pas a la suite entiere", file=sys.stderr)
            return 2
        source_ancienne = (ancien["mesure"].get("suite_rest") or {}).get("source")
        if source_ancienne != SOURCE_ID:
            print(f"--diff compare deux suites differentes : le rapport porte "
                  f"[{source_ancienne}], la mesure [{SOURCE_ID}]", file=sys.stderr)
            return 2

    divergences = None
    if RAPPORT_DIVERGENCES:
        # Lu **avant** la mesure, pour la meme raison que `--diff` : une
        # reference invalide doit arreter le runner tout de suite.
        try:
            divergences = lis_les_divergences(RAPPORT_DIVERGENCES)
        except (OSError, ValueError, KeyError, json.JSONDecodeError) as e:
            print(f"reference des divergences illisible "
                  f"[{RAPPORT_DIVERGENCES}] : {e}", file=sys.stderr)
            return 2

    serveur = Serveur(URL)
    info = repond(serveur)
    if info is None:
        print(f"la cible [{URL}] ne repond pas", file=sys.stderr)
        return 2
    print(f"== cible : {URL} — {moteur_de(info)} "
          f"{info.get('version', {}).get('number', '?')}")
    print(f"== suite REST de {SOURCE['nom']} {VERSION} ({SOURCE['licence']}), "
          f"{len(SUITES)} domaines")
    if divergences:
        ref = divergences["mesure"].get("cible") or {}
        print(f"== divergences tranchees par {RAPPORT_DIVERGENCES} — "
              f"{ref.get('moteur')} {ref.get('version_annoncee')}, "
              f"mesure du {divergences['mesure'].get('date')}")
    print()

    # Releve **avant** le premier nettoyage, et toujours : la reference est
    # l'etat de depart de la cible, pas le vide (voir `etat_de_depart`). Elle
    # sert a `--etat`, mais aussi au nettoyage, qui ne doit defaire que ce que
    # les cas ont pose (voir `balaye`).
    depart, muettes = etat_de_depart(serveur)
    ETAT_DE_DEPART = depart

    reference = None
    if ETAT_VERIFIE:
        reference = depart
        print("== etat verifie entre deux cas : " + ", ".join(reference))
        if muettes:
            print("   non verifiable (la cible ne sert pas la route) : "
                  + ", ".join(muettes))
        porte = {n: sorted(v) for n, v in reference.items() if v}
        if porte:
            print(f"   etat de depart de la cible : {json.dumps(porte)[:300]}")
        print()

    try:
        resultats, with_types = joue_la_suite(serveur, yaml, reference)
    except FuiteDEtat as e:
        print(f"\nFUITE D'ETAT — {e}\n\n"
              "   Un cas a laisse derriere lui un etat que `nettoie` n'attrape\n"
              "   pas : le verdict de tous les cas suivants en herite. La mesure\n"
              "   s'arrete ici, et aucun rapport n'est ecrit.", file=sys.stderr)
        return 2
    rapport = construis_rapport(resultats, info, with_types, divergences)
    affiche(rapport)
    nettoie(serveur)

    # Un serveur mort en cours de route rend tous les cas restants « echec » :
    # le rapport serait faux, et un rapport faux ecrit sur disque se fait
    # commiter. On refuse d'ecrire plutot que de mesurer un cadavre.
    if repond(serveur) is None:
        print(f"\nla cible [{URL}] a cesse de repondre pendant la mesure : "
              f"rapport non ecrit", file=sys.stderr)
        return 2

    if SORTIE_JSON:
        ecris_rapport(rapport, SORTIE_JSON)

    if ancien is not None:
        diff = compare_rapports(ancien, rapport)
        affiche_diff(ancien, RAPPORT_ANCIEN, diff)
        # Le cliquet : un cran qui ne remonte pas. Ce n'est pas une cible — il
        # ne dit rien du nombre d'echecs, seulement qu'il n'a pas augmente.
        if diff["echecs_apres"] > diff["echecs_avant"] or diff["regressions"]:
            print("\n   cliquet : REGRESSION")
            return 1
        print("\n   cliquet : ok")
        return 0

    return 1 if rapport["totaux"]["echecs"] else 0


if __name__ == "__main__":
    sys.exit(main())
