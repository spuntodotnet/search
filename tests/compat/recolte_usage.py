#!/usr/bin/env python3
"""Constituer un corpus de **vraies requetes** Elasticsearch, depuis des
sources publiques et citables.

    python3 tests/compat/recolte_usage.py                     # les quatre sources
    python3 tests/compat/recolte_usage.py --sources rally,doc
    python3 tests/compat/recolte_usage.py --sans-reseau       # rejoue les caches

# Pourquoi

Le compteur de conformance compte des **cas de test**, pas des usages : il met
`bool` + `match` (neuf applications sur dix) au meme rang qu'un
`significant_terms` avec script (une sur mille). Un taux non pondere ne dit donc
rien de ce qu'un projet reel peut brancher — ni dans un sens, ni dans l'autre.

Ce fichier ramasse la matiere qui permet de ponderer : des requetes que
quelqu'un a vraiment ecrites. Il ne conclut rien ; c'est
[`ponderation.py`](ponderation.py) qui compte et croise avec `compat.yaml`.

# Les quatre sources, et comment chacune est ramassee

  rally    les tracks de benchmark d'Elastic (elastic/rally-tracks, Apache-2.0).
           Chaque operation `search` porte un corps ecrit par Elastic pour
           mesurer un vrai moteur sur un vrai jeu de donnees.
  doc      la documentation de reference d'Elasticsearch 8.15.0 : tous les
           blocs `[source,console]` de `docs/reference/`, c'est-a-dire les
           exemples que lit celui qui ecrit une requete.
  clients  les depots des clients **officiels** (python, js, go, ruby) : leurs
           tests et leurs exemples, la ou une requete est ecrite pour etre
           executee.
  github   la recherche de code de GitHub, sur des **sites d'appel** (`.search(`
           en Python) et non sur des noms de clause : chercher
           `minimum_should_match` puis compter les `minimum_should_match`
           mesurerait la requete qu'on a posee, pas l'usage.

Rien n'est recopie dans le depot : les depots sont clones a la demande dans
`.corpus-usage/` (ignore par git), et seul le **corpus extrait** est publie
(`tests/compat/usage/corpus.jsonl`), avec la reference exacte de chaque requete —
depot, revision, fichier, ligne. C'est ce qui rend l'etude verifiable : chaque
ligne du corpus se reouvre chez sa source.

# Ce qu'est une requete, ici

Un enregistrement = un appel a l'API : `api` (le nom REST, resolu contre la
spec de la 8.15), `methode`, `chemin`, et `corps`. Une requete sans corps
compte quand meme : une route est un usage.
"""
import argparse
import ast
import json
import os
import re
import subprocess
import sys
import time
import warnings
import urllib.parse
import urllib.request

RACINE = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
CACHE = os.path.join(RACINE, ".corpus-usage")
SORTIE = os.path.join(RACINE, "tests", "compat", "usage")

ES_VERSION = "v8.15.0"
DEPOTS = {
    "rally": ("https://github.com/elastic/rally-tracks", "master", None),
    "es": ("https://github.com/elastic/elasticsearch", ES_VERSION,
           ["docs/reference", "rest-api-spec/src/main/resources/rest-api-spec/api"]),
    "py": ("https://github.com/elastic/elasticsearch-py", "main", None),
    "js": ("https://github.com/elastic/elasticsearch-js", "main", None),
    "go": ("https://github.com/elastic/go-elasticsearch", "main", None),
    "rb": ("https://github.com/elastic/elasticsearch-ruby", "main", None),
}

# Les clefs de premier niveau d'un corps de `_search` (spec 8.15 + doc). Elles
# servent a reconnaitre un corps de recherche **par sa forme**, sans chercher
# de nom de clause : c'est ce qui permet de ramasser un corps litteral dans du
# Go ou du JavaScript sans biaiser la mesure vers ce qu'on cherche.
CLEFS_SEARCH = {
    "query", "aggs", "aggregations", "size", "from", "sort", "_source", "fields",
    "docvalue_fields", "stored_fields", "script_fields", "highlight", "explain",
    "version", "seq_no_primary_term", "track_scores", "track_total_hits",
    "min_score", "post_filter", "rescore", "search_after", "slice", "suggest",
    "collapse", "indices_boost", "profile", "timeout", "terminate_after",
    "pit", "runtime_mappings", "knn", "stats", "ext",
}
# Il faut au moins une de celles-la : `{"size": 0}` tout seul n'est pas une
# requete, c'est un fragment.
CLEFS_SEARCH_FORTES = {"query", "aggs", "aggregations", "knn", "suggest", "collapse",
                       "post_filter", "search_after", "highlight", "sort"}

VERBES = ("GET", "PUT", "POST", "DELETE", "HEAD")


# --------------------------------------------------------------------------
# depots

def execute(cmd, cwd=None):
    subprocess.run(cmd, cwd=cwd, check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def clone(nom, sans_reseau=False):
    """Clone superficiel (et creux quand seul un sous-arbre nous interesse)."""
    url, ref, creux = DEPOTS[nom]
    dest = os.path.join(CACHE, nom)
    if not os.path.isdir(os.path.join(dest, ".git")):
        if sans_reseau:
            raise SystemExit(f"[{nom}] absent du cache et --sans-reseau demande")
        os.makedirs(CACHE, exist_ok=True)
        cmd = ["git", "clone", "--quiet", "--depth", "1", "--branch", ref]
        if creux:
            cmd += ["--filter=blob:none", "--sparse"]
        execute(cmd + [url, dest])
        if creux:
            execute(["git", "sparse-checkout", "set"] + creux, cwd=dest)
    sha = subprocess.run(["git", "rev-parse", "HEAD"], cwd=dest, check=True,
                         capture_output=True, text=True).stdout.strip()
    return dest, url, sha


def lien(url, sha, chemin, ligne=None):
    return f"{url}/blob/{sha}/{chemin}" + (f"#L{ligne}" if ligne else "")


def fichiers(racine, suffixes=None):
    for base, dirs, noms in os.walk(racine):
        dirs[:] = [d for d in dirs if d != ".git"]
        for nom in sorted(noms):
            if suffixes and not nom.endswith(suffixes):
                continue
            chemin = os.path.join(base, nom)
            if os.path.getsize(chemin) > 2_000_000:
                continue
            yield chemin, os.path.relpath(chemin, racine)


def propre(valeur):
    """Un JSON lu dans la nature peut porter un demi-surrogate (`\\uda6a`) :
    il traverse `json.loads` et fait echouer l'ecriture. On le remplace, on ne
    jette pas la requete pour un octet."""
    if isinstance(valeur, str):
        return valeur.encode("utf-8", "replace").decode("utf-8")
    if isinstance(valeur, dict):
        return {propre(k): propre(v) for k, v in valeur.items()}
    if isinstance(valeur, list):
        return [propre(v) for v in valeur]
    return valeur


def lit(chemin):
    with open(chemin, encoding="utf-8", errors="replace") as f:
        return f.read()


# --------------------------------------------------------------------------
# resolution d'une URL vers un nom d'API (la spec REST de la 8.15)

class Routes:
    """`POST /mon-index/_search` -> `search`.

    La spec d'API de la 8.15 donne, pour chaque API, ses chemins avec leurs
    parties variables. On la lit a l'envers. Le classement est celui d'ES :
    le chemin qui a le plus de segments **litteraux** gagne, sinon
    `/{index}/_doc/{id}` et `/_search/scroll/{id}` seraient interchangeables.
    """

    def __init__(self, dossier_api):
        self.motifs = []
        for nom in sorted(os.listdir(dossier_api)):
            if not nom.endswith(".json") or nom.startswith("_"):
                continue
            doc = json.load(open(os.path.join(dossier_api, nom), encoding="utf-8"))
            for api, corps in doc.items():
                for chemin in (corps.get("url") or {}).get("paths") or []:
                    segments = [s for s in chemin["path"].split("/") if s]
                    litteraux = sum(1 for s in segments if not s.startswith("{"))
                    self.motifs.append((litteraux, segments, api,
                                        [m.upper() for m in chemin.get("methods", [])]))
        self.motifs.sort(key=lambda m: -m[0])

    def api(self, methode, chemin):
        segments = [s for s in chemin.split("?")[0].split("/") if s]
        for _, motif, api, methodes in self.motifs:
            if len(motif) != len(segments) or (methode and methodes and methode not in methodes):
                continue
            for attendu, recu in zip(motif, segments):
                if attendu.startswith("{"):
                    # une partie variable ne mange pas un segment reserve
                    if recu.startswith("_") and recu not in ("_all",):
                        break
                elif attendu != recu:
                    break
            else:
                return api
        return None


# --------------------------------------------------------------------------
# extraction generique : un objet JSON litteral, dans n'importe quel texte

def objets_json(texte):
    """Tous les objets JSON **equilibres** du texte, avec leur position.

    Sert a ramasser un corps de requete ecrit en dur dans du Go, du Ruby ou un
    fichier de fixtures, sans interprete pour le langage hote. L'ancre est
    l'accolade, pas un nom de clause : la mesure n'est donc pas biaisee vers
    ce qu'on cherche.
    """
    decodeur = json.JSONDecoder()
    i = 0
    n = len(texte)
    while True:
        i = texte.find("{", i)
        if i < 0:
            return
        try:
            valeur, fin = decodeur.raw_decode(texte, i)
        except ValueError:
            i += 1
            continue
        if isinstance(valeur, dict) and valeur:
            yield valeur, i
            i = fin
        else:
            i += 1


def est_corps_search(obj):
    if not isinstance(obj, dict) or not obj:
        return False
    clefs = set(obj)
    return clefs <= CLEFS_SEARCH and bool(clefs & CLEFS_SEARCH_FORTES)


def ligne_de(texte, position):
    return texte.count("\n", 0, position) + 1


# --------------------------------------------------------------------------
# extraction Python : les sites d'appel d'un client

APPELS = {
    "search": "search", "count": "count", "msearch": "msearch", "scroll": "scroll",
    "index": "index", "create": "create", "update": "update", "delete": "delete",
    "get": "get", "mget": "mget", "bulk": "bulk", "update_by_query": "update_by_query",
    "delete_by_query": "delete_by_query", "reindex": "reindex", "explain": "explain",
    "field_caps": "field_caps", "termvectors": "termvectors",
}
# Les mots-clefs d'un appel 8.x qui composent le **corps** de la requete
# (`es.search(index=..., query=..., aggs=...)`), par opposition a ceux qui
# vont dans la query string.
CORPS_KWARGS = CLEFS_SEARCH | {"mappings", "settings", "aliases", "doc", "script",
                               "docs", "conflicts", "dest", "source", "from_"}
# Le client 8.x expose `_source` sous le nom `source` et `from` sous `from_`
# (l'un est reserve en Python, l'autre commence par un souligne). Garder le nom
# du client ferait un parametre de corps qui n'existe pas.
RENOMMES = {"source": "_source", "from_": "from"}


def requetes_python(texte):
    """Les appels `.search(...)` d'un fichier Python, corps compris.

    Deux formes, les deux vivantes : `es.search(body={...})` (7.x et debut 8.x)
    et `es.search(query={...}, aggs={...})` (8.x). Seul ce qui est **litteral**
    est ramasse : un corps construit par le programme n'est pas lisible ici, et
    l'inventer serait pire que de le manquer.
    """
    try:
        with warnings.catch_warnings():   # du code trouve dans la nature en emet
            warnings.simplefilter("ignore")
            arbre = ast.parse(texte)
    except (SyntaxError, ValueError):
        return
    for noeud in ast.walk(arbre):
        if not isinstance(noeud, ast.Call) or not isinstance(noeud.func, ast.Attribute):
            continue
        api = APPELS.get(noeud.func.attr)
        if not api:
            continue
        corps, kwargs = None, {}
        for mc in noeud.keywords:
            if mc.arg is None:
                continue
            try:
                valeur = ast.literal_eval(mc.value)
            except (ValueError, SyntaxError, TypeError):
                continue
            if mc.arg == "body":
                corps = valeur
            elif mc.arg in CORPS_KWARGS:
                nom = RENOMMES.get(mc.arg, mc.arg) if api != "reindex" else mc.arg
                kwargs[nom] = valeur
        if corps is None and kwargs:
            corps = kwargs
        if not isinstance(corps, dict) or not corps:
            continue
        yield api, corps, noeud.lineno


# --------------------------------------------------------------------------
# source 1 : les tracks Rally

JINJA_BLOC = re.compile(r"{%.*?%}", re.S)
JINJA_DEFAUT_TEXTE = re.compile(r"\"{{\s*[^}]*?default\(\s*'([^']*)'\s*\)[^}]*}}\"")
JINJA_DEFAUT_NOMBRE = re.compile(r"{{\s*[^}]*?default\(\s*([0-9.]+)\s*\)[^}]*}}")
JINJA_RESTE_TEXTE = re.compile(r"\"{{[^}]*}}\"")
JINJA_RESTE = re.compile(r"{{[^}]*}}")
VIRGULE_FINALE = re.compile(r",(\s*[}\]])")

# `operation-type` de Rally -> nom d'API REST. Ce qui n'est pas ici (esql, sql,
# vector-search sur une API qui n'existe pas en 8.15…) est garde avec son nom
# d'origine et compte comme route non resolue, jamais comme `search`.
OPERATIONS = {
    "search": "search", "paginated-search": "search", "scroll-search": "search",
    "composite-agg": "search", "bulk": "bulk", "raw-request": None,
    "create-index": "indices.create", "delete-index": "indices.delete",
    "put-settings": "indices.put_settings", "index-stats": "indices.stats",
    "force-merge": "indices.forcemerge", "refresh": "indices.refresh",
    "search-after-scroll": "search", "field-caps": "field_caps",
    "open-point-in-time": "open_point_in_time", "close-point-in-time": "close_point_in_time",
    "put-pipeline": "ingest.put_pipeline", "delete-pipeline": "ingest.delete_pipeline",
    "create-snapshot-repository": "snapshot.create_repository",
    "delete-snapshot-repository": "snapshot.delete_repository",
    "create-snapshot": "snapshot.create", "restore-snapshot": "snapshot.restore",
    "delete-snapshot": "snapshot.delete", "wait-for-snapshot-create": "snapshot.status",
    "wait-for-current-snapshots-create": "snapshot.status",
    "restore-snapshot-wait-for-completion": "snapshot.restore",
    "create-transform": "transform.put_transform", "start-transform": "transform.start_transform",
    "wait-for-transform": "transform.get_transform_stats",
    "delete-transform": "transform.delete_transform",
    "create-composable-template": "indices.put_index_template",
    "delete-composable-template": "indices.delete_index_template",
    "create-component-template": "cluster.put_component_template",
    "delete-component-template": "cluster.delete_component_template",
    "create-ml-datafeed": "ml.put_datafeed", "sql": "sql.query", "esql": "esql.query",
    "query": "search", "downsample": "indices.downsample",
}


def json_tolerant(texte):
    """Les tracks Rally sont du JSON **templatise** (Jinja2) : on remplace
    chaque expression par la valeur par defaut qu'elle declare, faute de quoi
    rien ne se parse."""
    texte = JINJA_BLOC.sub("", texte)
    texte = JINJA_DEFAUT_TEXTE.sub(lambda m: json.dumps(m.group(1)), texte)
    texte = JINJA_DEFAUT_NOMBRE.sub(lambda m: m.group(1), texte)
    texte = JINJA_RESTE_TEXTE.sub('"?"', texte)
    texte = JINJA_RESTE.sub("1", texte)
    texte = VIRGULE_FINALE.sub(r"\1", texte)
    for essai in (texte, "[" + texte + "]", "{" + texte + "}"):
        try:
            return json.loads(essai)
        except ValueError:
            continue
    return None


def source_rally(sans_reseau, routes):
    dest, url, sha = clone("rally", sans_reseau)
    vus = 0
    for chemin, relatif in fichiers(dest, (".json",)):
        texte = lit(chemin)
        doc = json_tolerant(texte)
        if doc is None:
            continue
        for op, position in operations_rally(doc, texte):
            vus += 1
            yield op | {"source": "rally",
                        "ref": lien(url, sha, relatif, position)}
    if not vus:
        raise SystemExit("rally : rien extrait, le format a change")


def operations_rally(doc, texte):
    """Une operation Rally, ou un corps de creation d'index."""
    piles = [doc]
    while piles:
        noeud = piles.pop()
        if isinstance(noeud, list):
            piles.extend(noeud)
            continue
        if not isinstance(noeud, dict):
            continue
        piles.extend(v for v in noeud.values() if isinstance(v, (dict, list)))
        if "operation-type" in noeud:
            type_op = noeud["operation-type"]
            api = OPERATIONS.get(type_op, f"rally:{type_op}")
            corps = noeud.get("body")
            if type_op == "raw-request":
                api = None
                chemin = noeud.get("path", "")
                yield {"api": None, "methode": noeud.get("method", "GET"),
                       "chemin": chemin, "corps": corps,
                       "origine": noeud.get("name", type_op)}, position_de(texte, noeud)
                continue
            if corps is None and api != "search":
                continue
            yield {"api": api, "methode": "POST", "chemin": None, "corps": corps,
                   "origine": noeud.get("name", type_op)}, position_de(texte, noeud)
        elif "mappings" in noeud and ("settings" in noeud or len(noeud) == 1):
            yield {"api": "indices.create", "methode": "PUT", "chemin": None,
                   "corps": noeud, "origine": "index"}, position_de(texte, noeud)


def position_de(texte, noeud):
    """La ligne ou commence cette operation, retrouvee par son nom. Approximatif
    et assume : c'est un lien de lecture, pas une preuve."""
    nom = noeud.get("name") if isinstance(noeud, dict) else None
    if isinstance(nom, str):
        i = texte.find(f'"{nom}"')
        if i >= 0:
            return ligne_de(texte, i)
    return None


# --------------------------------------------------------------------------
# source 2 : la documentation de reference 8.15

BLOC = re.compile(r"\[source,console\]\s*\n-{4,}\n(.*?)\n-{4,}", re.S)
CALLOUT = re.compile(r"\s*<\d+>\s*$")
# Une ligne de requete, c'est un verbe puis un chemin — sans espace. Exiger
# le `/` initial couterait 1 090 blocs : la doc ecrit `POST _aliases`.
DEBUT_CHEMIN = re.compile(r"^[/_%<a-zA-Z0-9][^\s]*$")


def source_doc(sans_reseau, routes):
    dest, url, sha = clone("es", sans_reseau)
    racine = os.path.join(dest, "docs", "reference")
    for chemin, relatif in fichiers(racine, (".asciidoc",)):
        texte = lit(chemin)
        relatif = os.path.join("docs", "reference", relatif)
        for bloc in BLOC.finditer(texte):
            debut = ligne_de(texte, bloc.start())
            for req in requetes_console(bloc.group(1)):
                req["api"] = routes.api(req["methode"], req["chemin"])
                yield req | {"source": "doc", "ref": lien(url, sha, relatif, debut)}


def requetes_console(bloc):
    """Un bloc `console` est une suite de `METHODE chemin` + corps, separes par
    des lignes vides. Le corps peut etre du NDJSON (`_bulk`, `_msearch`)."""
    courant = None
    tampon = []
    for brute in bloc.split("\n"):
        ligne = CALLOUT.sub("", brute.rstrip())
        if ligne.startswith("//") or ligne.startswith("#"):
            continue
        tete = ligne.split(" ", 1)
        if tete[0] in VERBES and len(tete) == 2 and DEBUT_CHEMIN.match(tete[1].strip()):
            if courant:
                yield fin_requete(courant, tampon)
            chemin = tete[1].strip()
            courant = {"methode": tete[0],
                       "chemin": chemin if chemin.startswith("/") else "/" + chemin}
            tampon = []
        elif courant is not None:
            tampon.append(ligne)
    if courant:
        yield fin_requete(courant, tampon)


def fin_requete(courant, tampon):
    texte = "\n".join(tampon).strip()
    corps, lignes = None, None
    if texte:
        try:
            corps = json.loads(texte)
        except ValueError:
            lues = []
            for ligne in texte.split("\n"):
                ligne = ligne.strip()
                if not ligne:
                    continue
                try:
                    lues.append(json.loads(ligne))
                except ValueError:
                    lues = None
                    break
            lignes = lues
    sortie = dict(courant)
    sortie["corps"] = corps
    if lignes:
        sortie["corps_lignes"] = lignes
    return sortie


# --------------------------------------------------------------------------
# source 3 : les clients officiels

# On ne ramasse que la ou une requete est **ecrite pour etre executee** : les
# tests et les exemples. Le reste d'un depot de client, c'est du transport.
DOSSIERS_CLIENTS = {
    "py": ["test_elasticsearch", "examples", "docs/examples"],
    "js": ["test", "docs/examples"],
    "go": ["_examples", "esapi/test", "typedapi", "docs/examples"],
    "rb": ["elasticsearch-api/spec", "docs/examples", "examples"],
}
SUFFIXES_TEXTE = (".py", ".js", ".ts", ".go", ".rb", ".json", ".asciidoc", ".yml", ".yaml", ".md")


def source_clients(sans_reseau, routes):
    for nom in ("py", "js", "go", "rb"):
        dest, url, sha = clone(nom, sans_reseau)
        for sous in DOSSIERS_CLIENTS[nom]:
            racine = os.path.join(dest, sous)
            if not os.path.isdir(racine):
                continue
            for chemin, relatif in fichiers(racine, SUFFIXES_TEXTE):
                relatif = os.path.join(sous, relatif)
                texte = lit(chemin)
                vus = set()
                if chemin.endswith(".py"):
                    for api, corps, ligne in requetes_python(texte):
                        vus.add(json.dumps(corps, sort_keys=True))
                        yield {"source": "clients", "api": api, "methode": None,
                               "chemin": None, "corps": corps, "client": nom,
                               "ref": lien(url, sha, relatif, ligne)}
                for obj, position in objets_json(texte):
                    if est_corps_search(obj) and json.dumps(obj, sort_keys=True) not in vus:
                        yield {"source": "clients", "api": "search", "methode": None,
                               "chemin": None, "corps": obj, "client": nom,
                               "ref": lien(url, sha, relatif, ligne_de(texte, position))}


# --------------------------------------------------------------------------
# source 4 : la recherche de code de GitHub

# Des ancres **neutres** : un site d'appel, jamais un nom de clause. Chercher
# `"minimum_should_match"` puis compter les `minimum_should_match` mesurerait
# la question posee, pas l'usage. Chaque requete est citee telle quelle dans
# l'etude, avec la date de la collecte : la recherche de code n'est pas
# reproductible a l'identique, seul le corpus publie l'est.
REQUETES_GITHUB = [
    '"es.search(index=" language:python',
    '"client.search(index=" language:python',
    '"es.search(body=" language:python',
    '"elasticsearch.search(index=" language:python',
    '".search(index=" "aggs" language:python',
    '"Elasticsearch(" ".search(" language:python',
    '"es.count(index=" language:python',
    '"helpers.scan(" language:python',
]
PAR_PAGE = 100


def gh_api(chemin, jeton=None):
    req = urllib.request.Request("https://api.github.com" + chemin)
    req.add_header("Accept", "application/vnd.github+json")
    if jeton:
        req.add_header("Authorization", f"Bearer {jeton}")
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)


def jeton_github():
    for var in ("GITHUB_TOKEN", "GH_TOKEN"):
        if os.environ.get(var):
            return os.environ[var]
    try:
        return subprocess.run(["gh", "auth", "token"], check=True, capture_output=True,
                              text=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return None


def source_github(sans_reseau, routes):
    if sans_reseau:
        return
    jeton = jeton_github()
    if not jeton:
        print("github : pas de jeton (gh auth login), source ignoree", file=sys.stderr)
        return
    vus = set()
    for i, q in enumerate(REQUETES_GITHUB):
        if i:
            time.sleep(7)  # 10 recherches de code par minute, pas plus
        try:
            reponse = gh_api(f"/search/code?q={urllib.parse.quote(q)}&per_page={PAR_PAGE}", jeton)
        except Exception as e:  # noqa: BLE001 — une source qui tombe ne doit pas tout arreter
            print(f"github : [{q}] a echoue ({e})", file=sys.stderr)
            continue
        for item in reponse.get("items", []):
            clef = (item["repository"]["full_name"], item["path"])
            if clef in vus:
                continue
            vus.add(clef)
            try:
                brut = fichier_github(item, jeton)
            except Exception:  # noqa: BLE001
                continue
            for api, corps, ligne in requetes_python(brut):
                yield {"source": "github", "api": api, "methode": None, "chemin": None,
                       "corps": corps, "requete_de_recherche": q,
                       "ref": f"https://github.com/{clef[0]}/blob/{item.get('sha', 'HEAD')}"
                              f"/{item['path']}#L{ligne}"}


def fichier_github(item, jeton):
    blob = gh_api(item["git_url"].replace("https://api.github.com", ""), jeton)
    import base64
    return base64.b64decode(blob["content"]).decode("utf-8", "replace")


# --------------------------------------------------------------------------

SOURCES = {"rally": source_rally, "doc": source_doc,
           "clients": source_clients, "github": source_github}


def sans_doublons(corpus):
    """Deux fois la meme requete **dans le meme fichier** ne sont qu'un usage.

    Un fichier de doc qui remonte son exemple de depart avant chaque variante,
    un track qui declare deux fois le meme corps sous deux noms : les compter
    deux fois ferait peser un copier-coller autant que deux requetes ecrites.
    Le meme corps dans deux fichiers, en revanche, compte deux fois — ce sont
    bien deux endroits ou quelqu'un l'a ecrit.
    """
    vus, sortie, ecartes = set(), [], 0
    for req in corpus:
        clef = (req.get("ref", "").split("#")[0],
                json.dumps([req.get("api"), req.get("corps"), req.get("chemin")],
                           sort_keys=True))
        if clef in vus:
            ecartes += 1
            continue
        vus.add(clef)
        sortie.append(req)
    return sortie, ecartes


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sources", default=",".join(SOURCES))
    ap.add_argument("--sortie", default=SORTIE)
    ap.add_argument("--sans-reseau", action="store_true")
    args = ap.parse_args()

    dest, _, _ = clone("es", args.sans_reseau)
    routes = Routes(os.path.join(dest, "rest-api-spec", "src", "main", "resources",
                                 "rest-api-spec", "api"))

    os.makedirs(args.sortie, exist_ok=True)
    corpus, doublons = [], 0
    meta = {"collecte": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "spec_api": ES_VERSION, "sources": {}}
    for nom in args.sources.split(","):
        nom = nom.strip()
        if nom not in SOURCES:
            raise SystemExit(f"source inconnue : {nom}")
        brutes = []
        for req in SOURCES[nom](args.sans_reseau, routes):
            req.setdefault("api", None)
            if req.get("chemin") and not req.get("api"):
                req["api"] = routes.api(req.get("methode"), req["chemin"])
            brutes.append(req)
        gardees, ecartes = sans_doublons(brutes)
        corpus.extend(gardees)
        doublons += ecartes
        meta["sources"][nom] = {"requetes": len(gardees), "doublons_ecartes": ecartes}
        if nom in ("rally", "doc"):
            _, url, sha = clone({"rally": "rally", "doc": "es"}[nom], True)
            meta["sources"][nom] |= {"depot": url, "revision": sha, "licence": "Apache-2.0"}
        elif nom == "clients":
            meta["sources"][nom]["depots"] = {}
            for c in ("py", "js", "go", "rb"):
                _, url, sha = clone(c, True)
                meta["sources"][nom]["depots"][c] = {"depot": url, "revision": sha}
        else:
            meta["sources"][nom]["requetes_de_recherche"] = REQUETES_GITHUB
        print(f"{nom:<8} {meta['sources'][nom]['requetes']:>6} requetes "
              f"({ecartes} doublons ecartes)", file=sys.stderr)

    meta["doublons_ecartes"] = doublons
    chemin = os.path.join(args.sortie, "corpus.jsonl")
    with open(chemin, "w", encoding="utf-8") as f:
        for req in corpus:
            f.write(json.dumps(propre(req), ensure_ascii=False, sort_keys=True) + "\n")
    meta["total"] = len(corpus)
    with open(os.path.join(args.sortie, "sources.json"), "w", encoding="utf-8") as f:
        json.dump(meta, f, ensure_ascii=False, indent=2)
        f.write("\n")
    print(f"== {len(corpus)} requetes -> {chemin}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
