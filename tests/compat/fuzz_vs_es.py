#!/usr/bin/env python3
"""Fuzzing differentiel : ce qui marche **ailleurs que sur les cas qu'on a ecrits**.

Tout le reste du harnais compare ferrite a Elasticsearch sur des questions que
*nous* avons posees — sauf la suite REST d'Elastic, qui pose celles d'Elastic
mais qui est figee et date de la 7.10. Le risque qui reste est exactement
celui-la : ferrite a ete construit contre un projet reel et contre une suite de
tests, et rien ne mesure ce qui se passe **en dehors** des combinaisons
auxquelles on a pense. Un test qu'on ecrit soi-meme porte la meme idee fausse
que le code qu'il teste.

Ce fichier tire au sort un mapping, des documents et des requetes **dans le
perimetre declare**, les envoie aux deux serveurs, et compare les reponses
normalisees. L'oracle est un vrai Elasticsearch 8.15 : ferrite n'est jamais
compare a l'idee qu'on se fait d'ES.

    python3 tests/compat/fuzz_vs_es.py [ferrite] [es] --cas 200
    python3 tests/compat/fuzz_vs_es.py --calibrer [es_a] [es_b] --cas 200
    python3 tests/compat/fuzz_vs_es.py --rejouer 1234567        # un cas, en detail
    python3 tests/compat/fuzz_vs_es.py --couverture             # ce qu'il fuzze, et pas

# Le perimetre est lu, pas reecrit

`compat.yaml` declare 178 capacites avec leur etat. Le generateur ne redit pas
cette liste : chaque **brique** (une clause du DSL, un type de champ, une
agregation, un parametre du corps) cite l'identifiant de la capacite qu'elle
exerce, et au demarrage le fuzzer :

  * **refuse de tourner** si une brique cite un identifiant inconnu — une
    capacite renommee casse le fuzzer bruyamment plutot que de le laisser
    fuzzer a cote ;
  * **n'emet pas** une brique dont la capacite est declaree `refuse` — fuzzer
    hors perimetre mesurerait le catalogue des refus, pas la compatibilite ;
  * **imprime ce qu'il ne couvre pas** (`--couverture`) : les capacites
    `supporte` ou `partiel` qu'aucune brique n'exerce. Un fuzzer qui ne dit pas
    ou il ne va pas se lit comme s'il allait partout.

# L'etalonnage vient avant la mesure

`--calibrer` fait tourner exactement la meme batterie contre **deux**
Elasticsearch. Tant qu'elle n'y est pas a zero divergence, ce que le fuzzer dit
de ferrite ne vaut rien : une divergence peut venir du generateur (une requete
dont la reponse n'est pas deterministe) ou de la normalisation (un champ qu'on
compare alors qu'il ne peut pas coincider) aussi bien que du moteur. Le repo a
paye trois fois cette lecon (voir CLAUDE.md, « etalonner l'instrument »).

Deux serveurs sont necessaires : la batterie **ecrit** (elle cree des index et
indexe), donc l'etalonner contre un seul serveur mesurerait la difference entre
« avant » et « apres ».

# Ce qui est neutralise avant comparaison, et pourquoi

Aucune exception n'est tacite. La liste est `NEUTRALISATIONS` plus bas ; en
resume :

  * `took` — une duree ne coincide pas ;
  * `_scroll_id` — un identifiant opaque, propre a chaque serveur ;
  * `_score` et `max_score` **en valeur** — BM25 est calcule par tantivy d'un
    cote et par Lucene de l'autre. C'est **l'ordre** qui est compare, avec la
    regle deja retenue par `diff_relevance.py` : une permutation ne compte pas
    si ES donne le meme score aux deux documents qu'elle echange ;
  * les flottants d'une agregation — compares a 1e-9 pres en relatif, deux
    sommes de doubles dans un ordre different ne sont pas bit-a-bit egales ;
  * le **corps** d'une erreur — pas son statut. ferrite nomme ses refus avec
    son propre type (`not_implemented_in_ferrite_exception`), expres : un
    client qui le voit sait que ce n'est pas sa requete qui est fautive. Le
    statut, lui, se compare, et une erreur d'un cote seulement est signalee.

Et trois choses que le **generateur** s'interdit, pour ne pas mesurer autre
chose que l'API :

  * `sort: ["_doc"]` — ES documente cet ordre comme celui du segment, pas comme
    une promesse d'API. Le comparer mesurerait le moteur de stockage ;
  * `float` a valeur quelconque — ES stocke un `float` sur 32 bits, ferrite le
    traduit en `f64`. Les valeurs tirees sont donc exactement representables en
    binary32 (multiples de 1/64), sans quoi *toute* agregation sur un `float`
    divergerait a la 8e decimale et noierait le signal. La question « faut-il
    tronquer a 32 bits ? » est une vraie question, elle a sa propre entree dans
    `docs/compat.md` — elle ne se traite pas au hasard ;
  * une pagination tronquante **sans ordre total**. Quand `size` coupe au
    milieu d'un paquet d'ex aequo, les deux serveurs ont le droit de rendre des
    documents differents : ce n'est pas une divergence, c'est une question mal
    posee. Le generateur pose donc soit `size >= |corpus|` (tout sort, l'ordre
    se compare entier), soit un tri qui finit par une cle unique.

# Les trois verdicts

Une divergence trouvee peut etre trois choses, et il faut pouvoir les
distinguer (la carte 28 a montre qu'un enonce de probleme pouvait etre faux) :

  * un **defaut de ferrite** — a corriger ;
  * une **divergence assumee** — a documenter dans `compat.yaml` ;
  * une **erreur du generateur** — la requete n'a pas de reponse deterministe,
    ou elle sort du perimetre declare.

Le fuzzer ne tranche pas : il rend le cas rejouable (`--rejouer <graine>`) et
imprime la requete, les deux reponses et l'ecart. C'est la mesure qui tranche.

Outil de developpement : exige un Elasticsearch 8.15 lance a cote (Docker).
"""
import argparse
import datetime
import json
import os
import random
import re
import struct
import sys
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import genere_compat  # noqa: E402

RACINE = genere_compat.RACINE
INDEX = "fuzz_vs_es"
TEMPLATE = "fuzz_vs_es_tpl"

# Ce qui est retire des reponses avant comparaison. Chaque entree porte sa
# raison : une neutralisation tacite est une divergence qu'on ne verra jamais.
NEUTRALISATIONS = {
    "took": "une duree ne coincide pas entre deux serveurs",
    "_scroll_id": "un identifiant opaque, propre a chaque serveur",
    "_score": "BM25 par tantivy d'un cote, par Lucene de l'autre — c'est l'ordre "
              "qui est compare, pas la valeur",
    "max_score": "meme raison que _score ; seule sa presence (null ou non) est "
                 "comparee",
    "_shards.failures[].reason": "le texte d'un echec de shard est propre au moteur",
    "_seq_no / _primary_term / _version": "des compteurs de replication, hors "
                                          "perimetre mono-noeud",
    "_ignored": "ES liste dans chaque hit les champs qu'un `ignore_above` a "
                "ecartes ; c'est une trace de l'indexation, pas un resultat",
    "mapping (parametres)": "le mapping rendu est compare sur `chemin -> type` : "
                            "c'est le type qui change les resultats, les "
                            "parametres autour sont propres a chaque moteur",
    "scroll.motif": "le motif d'un scroll refuse, comme tout corps d'erreur : il "
                    "est joint au rapport pour qu'on voie pourquoi, jamais "
                    "compare",
}


# ---------------------------------------------------------------------------
# HTTP
# ---------------------------------------------------------------------------


def http(base, method, path, body=None, brut=None):
    data = brut if brut is not None else (
        json.dumps(body).encode() if body is not None else None)
    req = urllib.request.Request(
        base + path, data=data, method=method,
        headers={"Content-Type": "application/x-ndjson" if brut is not None
                 else "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        corps = e.read()
        try:
            return e.code, json.loads(corps or b"{}")
        except json.JSONDecodeError:
            return e.code, {"raw": corps.decode(errors="replace")}


# ---------------------------------------------------------------------------
# Le perimetre : lu dans compat.yaml, jamais reecrit ici
# ---------------------------------------------------------------------------


class Perimetre:
    """Ce que compat.yaml autorise le generateur a produire.

    Une brique du generateur cite l'identifiant de la capacite qu'elle exerce.
    `jouable(id)` dit si elle a le droit de sortir ; `vu(id)` note qu'elle est
    sortie, pour le rapport de couverture."""

    def __init__(self):
        doc = genere_compat.charge()
        self.etats = {}
        for _, cap in genere_compat.capacites(doc):
            self.etats[cap["id"]] = cap["etat"]
        self.compte = {}

    def connu(self, cid):
        if cid not in self.etats:
            raise SystemExit(
                f"# la brique cite la capacite [{cid}], que compat.yaml ne "
                f"declare pas. Le fuzzer s'arrete plutot que de fuzzer a cote : "
                f"une capacite renommee doit casser bruyamment.")
        return cid

    def jouable(self, cid):
        return self.etats[self.connu(cid)] != "refuse"

    def vu(self, cid):
        self.compte[cid] = self.compte.get(cid, 0) + 1

    def couverture(self, briques):
        """(exercees, declarees mais jamais exercees) parmi supporte/partiel."""
        cites = {cid for cid in briques}
        tenues = {cid for cid, etat in self.etats.items() if etat != "refuse"}
        return sorted(cites & tenues), sorted(tenues - cites)


# Les capacites qu'une brique du generateur exerce. La cle est arbitraire, la
# valeur est un identifiant de compat.yaml — et il est verifie au demarrage.
BRIQUES = {
    # types de champ
    "champ.text": "type.text",
    "champ.keyword": "type.keyword",
    "champ.entier": "type.entiers",
    "champ.flottant": "type.flottants",
    "champ.boolean": "type.boolean",
    "champ.date": "type.date",
    "champ.object": "type.object",
    "champ.nested": "type.nested",
    "champ.multi_fields": "type.multi_fields",
    "champ.ignore_above": "type.ignore_above",
    "champ.analyzer": "type.analyzer",
    "champ.tableaux": "type.tableaux",
    "champ.null": "type.null",
    "champ.devine": "index.mapping_dynamique",
    # clauses
    "q.match_all": "dsl.match_all",
    "q.match_none": "dsl.match_none",
    "q.match": "dsl.match",
    "q.multi_match": "dsl.multi_match",
    "q.match_phrase": "dsl.match_phrase",
    "q.match_phrase_prefix": "dsl.match_phrase_prefix",
    "q.exists": "dsl.exists",
    "q.term": "dsl.term",
    "q.terms": "dsl.terms",
    "q.ids": "dsl.ids",
    "q.prefix": "dsl.prefix",
    "q.wildcard": "dsl.wildcard",
    "q.regexp": "dsl.regexp",
    "q.fuzzy": "dsl.fuzzy",
    "q.range": "dsl.range",
    "q.bool": "dsl.bool",
    "q.constant_score": "dsl.constant_score",
    "q.dis_max": "dsl.dis_max",
    "q.nested": "nested.clause",
    "q.nested.internes": "nested.clauses_internes",
    "q.nested.score_mode": "nested.score_mode",
    # corps de la recherche
    "corps.query": "recherche.query",
    "corps.from_size": "recherche.from_size",
    "corps.sort": "recherche.sort",
    "corps.source": "recherche.source",
    "corps.fields": "recherche.fields",
    "corps.docvalue_fields": "recherche.docvalue_fields",
    "corps.stored_fields": "recherche.stored_fields",
    "corps.track_total_hits": "recherche.track_total_hits",
    "corps.aggs": "recherche.aggs",
    "corps.scroll": "recherche.scroll",
    "corps.datemath": "datemath.now",
    # agregations
    "agg.metriques": "agg.metriques",
    "agg.terms": "agg.terms",
    "agg.range": "agg.range",
    "agg.histogram": "agg.histogram",
    "agg.date_histogram": "agg.date_histogram",
    "agg.filter": "agg.filter",
    "agg.sous": "agg.sous_agregations",
    # routes exercees a chaque cas
    "route.creer": "index.creation",
    "route.bulk": "ingestion.bulk",
    "route.search": "recherche.route",
    "route.count": "ingestion.count",
    "route.mapping": "index.mapping",
    "route.supprimer": "index.suppression",
    "route.refresh": "ingestion.refresh",
    # Les routes de description, posees sur le mapping tire au sort : c'est la
    # que `_field_caps` a le plus a dire, un mapping aleatoire melangeant
    # objets, `nested`, multi-fields et champs devines.
    "route.field_caps": "recherche.field_caps",
    "route.validate": "recherche.validate_query",
    "route.stats": "index.stats",
    "route.template": "index.templates",
    # Modifier ou purger par requete. Ces deux-la sont les seules briques qui
    # **ecrivent** : elles passent donc en dernier, une fois tout le reste
    # compare, et ce qu'elles laissent derriere elles est compare a son tour.
    "route.delete_by_query": "ingestion.delete_by_query",
    "route.update_by_query": "ingestion.update_by_query",
}

# Les analyzers integres, cites par leur propre capacite : un `analyzer` tire au
# sort sur un champ `text` exerce celui-la, pas « les analyzers » en general.
BRIQUES.update({f"analyzer.{a}": f"analyzer.{a}" for a in
                ("standard", "simple", "whitespace", "keyword", "stop",
                 "english", "french")})
# Les formes de date math que le generateur pose vraiment sur une borne.
BRIQUES.update({"datemath.decalage": "datemath.decalage",
                "datemath.arrondi": "datemath.arrondi"})
# Les analyzers a n-grammes declares par l'index : sans cette brique, la
# capacite sortirait declaree tenue sans que rien ne l'exerce.
BRIQUES.update({"analyse.custom": "analyse.custom",
                "analyse.tokenizers": "analyse.tokenizers",
                "analyse.filtres": "analyse.filtres",
                "analyse.ngram": "analyse.ngram"})


# ---------------------------------------------------------------------------
# Le generateur
# ---------------------------------------------------------------------------

MOTS = ["appareil", "modele", "version", "ecran", "batterie", "capteur",
        "silencieux", "portable", "compact", "leger", "bluetooth", "optique",
        "reduction", "bruit", "ambiant", "rapide", "solaire", "amovible",
        "aluminium", "acier", "verre", "tissu", "l'ascension", "elevee",
        "edition", "etendue", "hotel", "ecole"]
CLES = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "", "AlphA",
        "avec espace", "tiret-bas", "point.non", "accentue-e"]
ANALYZERS = ["standard", "simple", "whitespace", "keyword", "stop", "english",
             "french"]
FORMATS_DATE = [None, "yyyy-MM-dd", "yyyy-MM-dd HH:mm:ss", "epoch_millis",
                "strict_date_optional_time"]
# Les bornes exactes de chaque type entier d'ES : c'est la ou un moteur se
# trompe, pas au milieu.
BORNES = {
    "byte": (-128, 127),
    "short": (-32768, 32767),
    "integer": (-2147483648, 2147483647),
    "long": (-9223372036854775808, 9223372036854775807),
}
NUMERIQUES = list(BORNES) + ["float", "double"]
TRIABLES = ("keyword", "boolean", "date") + tuple(NUMERIQUES)
# La cle unique ajoutee a tout mapping : elle sert de dernier critere de tri, et
# c'est elle qui rend une pagination comparable entre deux moteurs.
TIEBREAK = "fuzz_cle"


def descendre(noeud, parts):
    """Les valeurs d'un chemin pointe dans un document, tableaux aplatis.

    Un chemin qui reste alors qu'on est deja sur une feuille est un
    multi-field (`titre.keyword`) : c'est la valeur du parent qui l'alimente."""
    if noeud is None:
        return []
    if isinstance(noeud, list):
        out = []
        for e in noeud:
            out.extend(descendre(e, parts))
        return out
    if isinstance(noeud, dict):
        return descendre(noeud[parts[0]], parts[1:]) if parts and parts[0] in noeud else []
    return [noeud]


def feuilles_de_nested(champs):
    """Les sous-champs d'un `nested`, tels qu'on les ecrirait depuis la racine.

    Ils ne sont pas dans la liste des champs interrogeables : une clause posee
    dessus depuis la racine est refusee (divergence assumee n° 10), et les y
    mettre remplirait le tirage de refus. Ils servent aux deux endroits ou le
    refus doit rester **exerce** : le tri et les agregations."""
    return [s for c in champs if c.ty == "nested" for s in c.sous]


class Champ:
    def __init__(self, nom, ty, mapping, chemin=None, sous=None):
        self.nom = nom          # nom court
        self.chemin = chemin or nom   # chemin pointe, tel qu'on l'interroge
        self.ty = ty
        self.mapping = mapping  # ce qui part dans `properties` (None si devine)
        self.sous = sous or []  # pour object / nested

    def __repr__(self):
        return f"<{self.chemin}:{self.ty}>"


class Generateur:
    """Un mapping, des documents et des requetes, tires d'une graine.

    La meme graine rend exactement les memes trois choses : une divergence se
    rejoue par `--rejouer <graine>`."""

    def __init__(self, graine, perimetre):
        self.rng = random.Random(graine)
        self.p = perimetre
        self.graine = graine
        # Ce que `settings` porte en plus, et les analyzers que l'index
        # declare — remplis par `mapping()`.
        self.reglages = {}
        self.analyzers_declares = []

    def brique(self, nom):
        """Note qu'une brique est sortie, et dit si elle a le droit."""
        cid = BRIQUES[nom]
        if not self.p.jouable(cid):
            return False
        self.p.vu(cid)
        return True

    # -- mapping ------------------------------------------------------------

    def mapping(self):
        """Un `properties` tire au sort, plus la liste des champs interrogeables.

        `TIEBREAK` est ajoute a tous les mappings et vaut l'identifiant du
        document : c'est la **cle unique** qui rend un tri total, donc une
        pagination comparable (voir l'entete). Il ne sort jamais dans une
        requete generee, pour ne pas biaiser le tirage."""
        rng = self.rng
        self._analyse_declaree()
        props, champs = {TIEBREAK: {"type": "keyword"}}, []
        noms = ["a", "b", "c", "d", "e", "f", "g", "h"]
        rng.shuffle(noms)
        for nom in noms[:rng.randint(4, 7)]:
            forme = rng.choices(
                ["text", "keyword", "entier", "flottant", "boolean", "date",
                 "object", "nested", "devine"],
                weights=[14, 16, 10, 8, 6, 10, 8, 8, 6])[0]
            if forme == "devine":
                # Pas de declaration : le champ n'existe que par ce que les
                # documents en disent. C'est le mapping dynamique — et ES y
                # devine `text` **plus** un sous-champ `.keyword`, jamais un
                # `keyword` seul. Le modeler autrement ferait poser au
                # generateur des requetes hors perimetre (un `range` sur ce
                # qui est en fait un `text`), et ce serait le fuzzer qui aurait
                # tort, pas ferrite.
                self.brique("champ.devine")
                ty = rng.choice(["text_devine", "text_devine", "long",
                                 "double", "boolean"])
                champs.append(Champ(nom, ty, None))
                if ty == "text_devine":
                    champs.append(Champ("keyword", "keyword", None,
                                        chemin=f"{nom}.keyword"))
                continue
            m, ch = getattr(self, f"_champ_{forme}")(nom)
            props[nom] = m
            champs.extend(ch)
        return props, champs

    def _analyse_declaree(self):
        """Une section `analysis` tiree au sort — deux analyzers a n-grammes.

        Un tokenizer d'un cote, un filtre de l'autre : ce ne sont pas les memes
        positions (le tokenizer avance a chaque gramme, le filtre pose tous les
        grammes d'un mot a la position de ce mot), donc pas les memes reponses
        a un `match_phrase`."""
        self.reglages, self.analyzers_declares = {}, []
        rng = self.rng
        if not (rng.random() < 0.3 and self.brique("analyse.ngram")):
            return
        self.brique("analyse.custom")
        self.brique("analyse.tokenizers")
        self.brique("analyse.filtres")
        mini = rng.randint(1, 3)
        tok = {"type": rng.choice(["ngram", "edge_ngram"]),
               "min_gram": mini, "max_gram": mini + rng.randint(0, 4)}
        if rng.random() < 0.5:
            tok["token_chars"] = rng.choice(
                [["letter"], ["letter", "digit"], ["letter", "digit", "punctuation"], []])
        mini = rng.randint(1, 3)
        filt = {"type": rng.choice(["ngram", "edge_ngram"]),
                "min_gram": mini, "max_gram": mini + rng.randint(0, 3)}
        if filt["type"] == "edge_ngram" and rng.random() < 0.3:
            filt["side"] = "back"
        if rng.random() < 0.3:
            filt["preserve_original"] = True
        self.reglages = {
            # L'ecart par defaut est 1 : sans ce reglage, la moitie des tirages
            # serait refusee des deux cotes et ne mesurerait rien.
            "max_ngram_diff": 12,
            "analysis": {
                "tokenizer": {"tk": tok},
                "filter": {"fl": filt},
                "analyzer": {
                    "ng_tok": {"type": "custom", "tokenizer": "tk",
                               "filter": ["lowercase"]},
                    "ng_filtre": {"type": "custom", "tokenizer": "standard",
                                  "filter": ["lowercase", "fl"]},
                },
            },
        }
        self.analyzers_declares = ["ng_tok", "ng_filtre"]

    def _champ_text(self, nom):
        self.brique("champ.text")
        m = {"type": "text"}
        if self.rng.random() < 0.4 and self.brique("champ.analyzer"):
            a = self.rng.choice(ANALYZERS + self.analyzers_declares)
            if a in ANALYZERS:
                self.brique(f"analyzer.{a}")
            m["analyzer"] = a
        champs = [Champ(nom, "text", m)]
        if self.rng.random() < 0.5 and self.brique("champ.multi_fields"):
            sous = {"type": "keyword"}
            if self.rng.random() < 0.5 and self.brique("champ.ignore_above"):
                sous["ignore_above"] = self.rng.choice([8, 32, 256])
            m["fields"] = {"keyword": sous}
            champs.append(Champ("keyword", "keyword", sous, chemin=f"{nom}.keyword"))
        return m, champs

    def _champ_keyword(self, nom):
        self.brique("champ.keyword")
        m = {"type": "keyword"}
        if self.rng.random() < 0.3 and self.brique("champ.ignore_above"):
            m["ignore_above"] = self.rng.choice([4, 12, 256])
        return m, [Champ(nom, "keyword", m)]

    def _champ_entier(self, nom):
        self.brique("champ.entier")
        ty = self.rng.choice(list(BORNES))
        return {"type": ty}, [Champ(nom, ty, {"type": ty})]

    def _champ_flottant(self, nom):
        self.brique("champ.flottant")
        ty = self.rng.choice(["float", "double"])
        return {"type": ty}, [Champ(nom, ty, {"type": ty})]

    def _champ_boolean(self, nom):
        self.brique("champ.boolean")
        return {"type": "boolean"}, [Champ(nom, "boolean", {"type": "boolean"})]

    def _champ_date(self, nom):
        self.brique("champ.date")
        m = {"type": "date"}
        fmt = self.rng.choice(FORMATS_DATE)
        if fmt:
            m["format"] = fmt
        return m, [Champ(nom, "date", m)]

    def _feuilles(self, prefixe, combien):
        """Les feuilles d'un `object` ou d'un `nested`, sans `text` ni imbrique."""
        props, champs = {}, []
        for sous in ["x", "y", "z"][:combien]:
            ty = self.rng.choice(["keyword", "long", "double", "boolean", "date"])
            props[sous] = {"type": ty}
            champs.append(Champ(sous, ty, props[sous],
                                chemin=f"{prefixe}.{sous}"))
        return props, champs

    def _champ_object(self, nom):
        self.brique("champ.object")
        props, champs = self._feuilles(nom, self.rng.randint(1, 3))
        m = {"properties": props}
        if self.rng.random() < 0.5:
            m["type"] = "object"
        return m, [Champ(nom, "object", m, sous=champs)] + champs

    def _champ_nested(self, nom):
        self.brique("champ.nested")
        props, champs = self._feuilles(nom, self.rng.randint(1, 3))
        m = {"type": "nested", "properties": props}
        return m, [Champ(nom, "nested", m, sous=champs)]

    # -- valeurs ------------------------------------------------------------

    def _mot(self):
        return self.rng.choice(MOTS)

    def _texte(self):
        r = self.rng.random()
        if r < 0.05:
            return ""
        if r < 0.1:
            return self.rng.choice(["Ecran, capteur.", "L'ascension : 3 !",
                                    "  espaces   multiples  ", "UPPER lower"])
        return " ".join(self._mot() for _ in range(self.rng.randint(1, 6)))

    def _valeur_simple(self, ty):
        rng = self.rng
        if ty in ("text", "text_devine"):
            return self._texte()
        if ty == "keyword":
            r = rng.random()
            if r < 0.08:
                # Au-dela d'un `ignore_above`, la valeur reste dans _source
                # sans etre indexee : la longueur est le sujet.
                return "z" * rng.choice([5, 20, 300])
            return rng.choice(CLES)
        if ty in BORNES:
            lo, hi = BORNES[ty]
            # Les bornes du type sont tirees exprès : c'est la que les moteurs
            # different (debordement, coercition).
            return rng.choice([lo, hi, 0, -1, 1, rng.randint(lo // 2, hi // 2)])
        if ty in ("float", "double"):
            # Multiples de 1/64 : exactement representables en binary32, donc
            # un `float` (32 bits chez ES, f64 chez ferrite) ne peut pas etre
            # la source d'un ecart de 8e decimale. Voir l'entete.
            return rng.choice([0.0, -0.5, 1.0, 1024.0,
                               rng.randint(-4096 * 64, 4096 * 64) / 64.0])
        if ty == "boolean":
            return rng.choice([True, False])
        if ty == "date":
            return self._date()
        raise AssertionError(f"type sans valeur : {ty}")

    def _date(self, fmt=None):
        rng = self.rng
        an, mois, jour = rng.randint(2023, 2026), rng.randint(1, 12), rng.randint(1, 28)
        h, mn, s = rng.randint(0, 23), rng.randint(0, 59), rng.randint(0, 59)
        if fmt == "yyyy-MM-dd":
            return f"{an:04d}-{mois:02d}-{jour:02d}"
        if fmt == "yyyy-MM-dd HH:mm:ss":
            return f"{an:04d}-{mois:02d}-{jour:02d} {h:02d}:{mn:02d}:{s:02d}"
        if fmt == "epoch_millis":
            return rng.randint(1_600_000_000_000, 1_800_000_000_000)
        # `strict_date_optional_time` : les deux formes qu'un client ecrit.
        if rng.random() < 0.5:
            return f"{an:04d}-{mois:02d}-{jour:02d}"
        return f"{an:04d}-{mois:02d}-{jour:02d}T{h:02d}:{mn:02d}:{s:02d}.000Z"

    def valeur(self, champ):
        """La valeur d'un champ dans un document, bords compris."""
        rng = self.rng
        ty = champ.ty
        if ty == "date":
            fmt = (champ.mapping or {}).get("format")
            base = lambda: self._date(fmt)  # noqa: E731
        else:
            base = lambda: self._valeur_simple(ty)  # noqa: E731
        r = rng.random()
        if r < 0.06:
            self.brique("champ.null")
            return None
        if r < 0.10:
            self.brique("champ.tableaux")
            return []                       # tableau vide : absent, chez ES
        if r < 0.22:
            self.brique("champ.tableaux")
            return [base() for _ in range(rng.randint(1, 3))]
        if r < 0.25:
            self.brique("champ.tableaux")
            # Tableau heterogene : une valeur nulle au milieu, qu'ES ignore.
            return [base(), None, base()]
        return base()

    def document(self, champs):
        rng = self.rng
        doc = {}
        for c in champs:
            if c.chemin.count(".") and c.ty != "nested":
                continue                    # une feuille est ecrite par son parent
            if rng.random() < 0.12:
                continue                    # champ absent
            if c.ty == "object":
                doc[c.nom] = {s.nom: self.valeur(s) for s in c.sous
                              if rng.random() < 0.8}
            elif c.ty == "nested":
                doc[c.nom] = [
                    {s.nom: self.valeur(s) for s in c.sous if rng.random() < 0.85}
                    for _ in range(rng.randint(0, 3))
                ]
            else:
                doc[c.chemin] = self.valeur(c)
        return doc

    def documents(self, champs, combien):
        docs = []
        for i in range(combien):
            doc = self.document(champs)
            doc[TIEBREAK] = f"d{i:03d}"
            docs.append((f"d{i:03d}", doc))
        return docs

    # -- requetes -----------------------------------------------------------

    def _termes(self, champ, docs):
        """Des valeurs qui existent vraiment dans le corpus.

        Tirer une valeur au hasard rendrait presque toujours zero document : la
        moitie de l'interet d'une requete est qu'elle ramene quelque chose."""
        vus = []
        for _, doc in docs:
            vus.extend(descendre(doc, champ.chemin.split(".")))
        return vus

    def _valeur_pour(self, champ, docs):
        vus = self._termes(champ, docs)
        if vus and self.rng.random() < 0.85:
            v = self.rng.choice(vus)
            if champ.ty in ("text", "text_devine") and isinstance(v, str) and v:
                # Sur un `text`, un terme indexe est un mot, pas la phrase.
                mots = v.split()
                return self.rng.choice(mots) if mots else v
            return v
        return self._valeur_simple("keyword" if champ.ty in ("text", "text_devine")
                                   else champ.ty)

    def feuille(self, champs, docs, profondeur=0):
        """Une clause du DSL, choisie selon le type du champ."""
        rng = self.rng
        interrogeables = [c for c in champs if c.ty not in ("object", "nested")]
        nesteds = [c for c in champs if c.ty == "nested"]

        choix = ["match_all", "match_none", "exists", "term", "terms", "ids"]
        if interrogeables:
            choix += ["term", "terms", "range", "prefix", "wildcard", "regexp",
                      "match", "fuzzy"]
        if any(c.ty in ("text", "text_devine") for c in champs):
            choix += ["match", "match_phrase", "match_phrase_prefix", "multi_match"]
        if nesteds and profondeur == 0:
            choix += ["nested", "nested"]
        if profondeur < 2:
            choix += ["bool", "bool", "constant_score", "dis_max"]

        for _ in range(8):
            quoi = rng.choice(choix)
            q = getattr(self, f"_q_{quoi}")(champs, docs, profondeur)
            if q is not None:
                return q
        return {"match_all": {}}

    def _q_match_all(self, champs, docs, prof):
        if not self.brique("q.match_all"):
            return None
        q = {}
        if self.rng.random() < 0.3:
            q["boost"] = self.rng.choice([0.5, 2.0])
        return {"match_all": q}

    def _q_match_none(self, champs, docs, prof):
        return {"match_none": {}} if self.brique("q.match_none") else None

    def _q_exists(self, champs, docs, prof):
        if not self.brique("q.exists"):
            return None
        c = self.rng.choice([c for c in champs if c.ty != "object"])
        return {"exists": {"field": c.chemin}}

    def _q_ids(self, champs, docs, prof):
        if not self.brique("q.ids"):
            return None
        n = min(len(docs), self.rng.randint(1, 4))
        return {"ids": {"values": [i for i, _ in self.rng.sample(docs, n)]}}

    def _champ_sauf(self, champs, types):
        cs = [c for c in champs if c.ty in types]
        return self.rng.choice(cs) if cs else None

    def _q_term(self, champs, docs, prof):
        if not self.brique("q.term"):
            return None
        c = self._champ_sauf(champs, ("keyword", "boolean", "date", "text",
                                      "text_devine") + tuple(NUMERIQUES))
        if c is None:
            return None
        v = self._valeur_pour(c, docs)
        if self.rng.random() < 0.3:
            return {"term": {c.chemin: {"value": v, "boost": 2.0}}}
        return {"term": {c.chemin: v}}

    def _q_terms(self, champs, docs, prof):
        if not self.brique("q.terms"):
            return None
        c = self._champ_sauf(champs, ("keyword", "boolean", "date", "text",
                                      "text_devine") + tuple(NUMERIQUES))
        if c is None:
            return None
        n = self.rng.randint(1, 3)
        return {"terms": {c.chemin: [self._valeur_pour(c, docs) for _ in range(n)]}}

    def _q_range(self, champs, docs, prof):
        if not self.brique("q.range"):
            return None
        # Pas de `range` sur un `text` : compat.yaml le refuse explicitement.
        c = self._champ_sauf(champs, ("keyword", "boolean", "date") + tuple(NUMERIQUES))
        if c is None:
            return None
        if c.ty == "date":
            return {"range": {c.chemin: self._bornes_date(c, docs)}}
        a, b = self._valeur_pour(c, docs), self._valeur_pour(c, docs)
        if isinstance(a, bool) or isinstance(b, bool):
            a, b = False, True
        bas, haut = self.rng.choice([("gte", "lte"), ("gt", "lt"),
                                     ("gte", "lt"), ("gt", "lte")])
        try:
            a, b = min(a, b), max(a, b)
        except TypeError:
            return None
        q = {}
        if self.rng.random() < 0.8:
            q[bas] = a
        if self.rng.random() < 0.8 or not q:
            q[haut] = b
        return {"range": {c.chemin: q}}

    def _bornes_date(self, c, docs):
        rng = self.rng
        fmt = (c.mapping or {}).get("format")
        if rng.random() < 0.3 and self.brique("corps.datemath"):
            # Le date math est resolu cote serveur : `now` n'est pas une date.
            self.brique("datemath.decalage")
            bas = rng.choice(["now-1y", "now-30d/d", "now/d", "now-6M"])
            haut = rng.choice(["now", "now+1y", "now+1d/d"])
            if "/" in bas or "/" in haut:
                self.brique("datemath.arrondi")
            # Un `format` explicite ne s'applique pas a `now` : l'expression
            # reste ancree sur `now`, jamais sur une date ecrite au format du
            # champ, pour ne pas melanger deux questions.
            return {rng.choice(["gte", "gt"]): bas,
                    rng.choice(["lte", "lt"]): haut}
        a, b = self._date(fmt), self._date(fmt)
        if isinstance(a, str) and isinstance(b, str):
            a, b = min(a, b), max(a, b)
        elif isinstance(a, int) and isinstance(b, int):
            a, b = min(a, b), max(a, b)
        return {rng.choice(["gte", "gt"]): a, rng.choice(["lte", "lt"]): b}

    def _q_prefix(self, champs, docs, prof):
        if not self.brique("q.prefix"):
            return None
        c = self._champ_sauf(champs, ("keyword", "text", "text_devine"))
        if c is None:
            return None
        v = self._valeur_pour(c, docs)
        if not isinstance(v, str) or not v:
            return None
        q = {"value": v[:self.rng.randint(1, max(1, len(v)))]}
        if self.rng.random() < 0.3:
            q["case_insensitive"] = True
        return {"prefix": {c.chemin: q}}

    def _q_wildcard(self, champs, docs, prof):
        if not self.brique("q.wildcard"):
            return None
        c = self._champ_sauf(champs, ("keyword", "text", "text_devine"))
        if c is None:
            return None
        v = self._valeur_pour(c, docs)
        if not isinstance(v, str) or not v:
            return None
        motif = self.rng.choice([v[:2] + "*", "*" + v[-2:], v[:1] + "?" + v[2:],
                                 "*" + v[1:3] + "*"])
        q = {"value": motif}
        if self.rng.random() < 0.3:
            q["case_insensitive"] = True
        return {"wildcard": {c.chemin: q}}

    def _q_regexp(self, champs, docs, prof):
        if not self.brique("q.regexp"):
            return None
        c = self._champ_sauf(champs, ("keyword", "text", "text_devine"))
        if c is None:
            return None
        v = self._valeur_pour(c, docs)
        if not isinstance(v, str) or not v or not v.isalnum():
            return None
        # Des motifs construits sur la donnee, dans la syntaxe **Lucene**
        # (ancree des deux cotes). Les operateurs `~ & <> #` ne sont pas tires :
        # compat.yaml les refuse explicitement.
        motif = self.rng.choice([v[:2] + ".*", ".*" + v[-2:], v,
                                 f"({v}|{self._mot()})", "[a-z].*",
                                 v[:1] + "[a-z]*"])
        q = {"value": motif}
        if self.rng.random() < 0.3:
            q["case_insensitive"] = True
        return {"regexp": {c.chemin: q}}

    def _q_fuzzy(self, champs, docs, prof):
        if not self.brique("q.fuzzy"):
            return None
        # Une fois sur dix, sur un champ qui n'est pas une chaine : une distance
        # d'edition n'y a pas de sens, et les deux serveurs doivent refuser.
        # C'est par ce trou que ferrite rendait « zero document » en 200.
        if self.rng.random() < 0.1:
            c = self._champ_sauf(champs, ("boolean", "date") + tuple(NUMERIQUES))
            if c is not None:
                return {"fuzzy": {c.chemin: {"value": "20"}}}
        c = self._champ_sauf(champs, ("keyword", "text", "text_devine"))
        if c is None:
            return None
        v = self._valeur_pour(c, docs)
        if not isinstance(v, str) or not v:
            return None
        q = {"value": v}
        if self.rng.random() < 0.6:
            q["fuzziness"] = self.rng.choice(["AUTO", 0, 1, 2])
        if self.rng.random() < 0.3:
            q["transpositions"] = self.rng.choice([True, False])
        return {"fuzzy": {c.chemin: q}}

    def _q_match(self, champs, docs, prof):
        if not self.brique("q.match"):
            return None
        c = self._champ_sauf(champs, ("text", "text_devine", "keyword",
                                      "boolean", "date") + tuple(NUMERIQUES))
        if c is None:
            return None
        v = self._valeur_pour(c, docs)
        if c.ty not in ("text", "text_devine", "keyword") and self.rng.random() < 0.15:
            # Une valeur que le champ ne sait pas lire : sans `lenient`, les
            # deux serveurs doivent refuser ; avec, ES ecarte le champ. C'est
            # exactement la frontiere que `lenient` ne doit pas depasser.
            v = "pas_une_valeur_de_ce_type"
        q = {"query": v}
        if self.rng.random() < 0.4:
            q["operator"] = self.rng.choice(["or", "and"])
        if self.rng.random() < 0.3:
            q["lenient"] = True
        return {"match": {c.chemin: q}}

    def _q_match_phrase(self, champs, docs, prof):
        if not self.brique("q.match_phrase"):
            return None
        c = self._champ_sauf(champs, ("text", "text_devine"))
        if c is None:
            return None
        vus = [v for v in self._termes(c, docs) if isinstance(v, str) and v.strip()]
        if not vus:
            return None
        mots = self.rng.choice(vus).split()
        n = min(len(mots), self.rng.randint(1, 3))
        debut = self.rng.randint(0, len(mots) - n)
        return {"match_phrase": {c.chemin: " ".join(mots[debut:debut + n])}}

    def _q_match_phrase_prefix(self, champs, docs, prof):
        if not self.brique("q.match_phrase_prefix"):
            return None
        c = self._champ_sauf(champs, ("text", "text_devine"))
        if c is None:
            return None
        vus = [v for v in self._termes(c, docs) if isinstance(v, str) and v.strip()]
        if not vus:
            return None
        mots = self.rng.choice(vus).split()
        n = min(len(mots), self.rng.randint(1, 2))
        texte = " ".join(mots[:n])
        q = {"query": texte[:max(1, len(texte) - self.rng.randint(0, 2))]}
        if self.rng.random() < 0.3:
            q["max_expansions"] = self.rng.choice([1, 5, 50])
        return {"match_phrase_prefix": {c.chemin: q}}

    def _q_multi_match(self, champs, docs, prof):
        if not self.brique("q.multi_match"):
            return None
        cs = [c for c in champs if c.ty in ("text", "text_devine", "keyword")]
        if not cs:
            return None
        n = min(len(cs), self.rng.randint(1, 3))
        choisis = self.rng.sample(cs, n)
        fields = [c.chemin + (f"^{self.rng.choice([2, 3])}"
                              if self.rng.random() < 0.3 else "")
                  for c in choisis]
        if self.rng.random() < 0.25:
            # Un champ que rien ne mappe : c'est le cas de la barre de recherche
            # qui balaie un champ jamais rempli. Il ne doit pas vider la clause.
            fields.append("champ_jamais_mappe")
        q = {"query": self._valeur_pour(choisis[0], docs), "fields": fields}
        if self.rng.random() < 0.5:
            q["type"] = self.rng.choice(["best_fields", "most_fields", "phrase",
                                         "phrase_prefix"])
        if self.rng.random() < 0.3:
            q["operator"] = self.rng.choice(["or", "and"])
        if self.rng.random() < 0.3:
            q["lenient"] = True
        if self.rng.random() < 0.2:
            q["tie_breaker"] = 0.3
        return {"multi_match": q}

    def _q_bool(self, champs, docs, prof):
        if not self.brique("q.bool"):
            return None
        rng = self.rng
        q = {}
        for section in ("must", "should", "filter", "must_not"):
            if rng.random() < 0.45:
                q[section] = [self.feuille(champs, docs, prof + 1)
                              for _ in range(rng.randint(1, 2))]
        if not q:
            q["must"] = [self.feuille(champs, docs, prof + 1)]
        if "should" in q and rng.random() < 0.5:
            q["minimum_should_match"] = rng.choice(
                [1, 2, "1", "50%", "-25%", "2<70%", "-1"])
        if rng.random() < 0.2:
            q["boost"] = 1.5
        return {"bool": q}

    def _q_constant_score(self, champs, docs, prof):
        if not self.brique("q.constant_score"):
            return None
        q = {"filter": self.feuille(champs, docs, prof + 1)}
        if self.rng.random() < 0.5:
            q["boost"] = self.rng.choice([1.0, 3.0])
        return {"constant_score": q}

    def _q_dis_max(self, champs, docs, prof):
        if not self.brique("q.dis_max"):
            return None
        q = {"queries": [self.feuille(champs, docs, prof + 1)
                         for _ in range(self.rng.randint(1, 3))]}
        if self.rng.random() < 0.5:
            q["tie_breaker"] = self.rng.choice([0.0, 0.3, 1.0])
        return {"dis_max": q}

    def _q_nested(self, champs, docs, prof):
        if not self.brique("q.nested"):
            return None
        nesteds = [c for c in champs if c.ty == "nested"]
        if not nesteds:
            return None
        c = self.rng.choice(nesteds)
        self.brique("q.nested.internes")
        interne = self._interne_nested(c, docs, 0)
        q = {"path": c.chemin, "query": interne}
        if self.rng.random() < 0.3 and self.brique("q.nested.score_mode"):
            q["score_mode"] = self.rng.choice(["none", "avg"])
        return {"nested": q}

    def _interne_nested(self, parent, docs, prof):
        """Une clause interne de `nested` : pas de `text`, pas de `nested`."""
        rng = self.rng
        sous = parent.sous
        quoi = rng.choice(["term", "terms", "range", "exists", "prefix", "bool"]
                          if prof == 0 else
                          ["term", "terms", "range", "exists", "prefix"])
        c = rng.choice(sous)
        if quoi == "bool":
            q = {}
            for section in ("must", "should", "filter", "must_not"):
                if rng.random() < 0.45:
                    q[section] = [self._interne_nested(parent, docs, prof + 1)]
            if not q:
                q["must"] = [self._interne_nested(parent, docs, prof + 1)]
            if "should" in q and rng.random() < 0.5:
                q["minimum_should_match"] = rng.choice([1, 2, "50%", "-25%"])
            return {"bool": q}
        if quoi == "exists":
            return {"exists": {"field": c.chemin}}
        if quoi == "range":
            if c.ty == "date":
                return {"range": {c.chemin: self._bornes_date(c, docs)}}
            a, b = self._valeur_pour(c, docs), self._valeur_pour(c, docs)
            if isinstance(a, bool) or isinstance(b, bool):
                a, b = False, True
            try:
                a, b = min(a, b), max(a, b)
            except TypeError:
                return {"exists": {"field": c.chemin}}
            return {"range": {c.chemin: {"gte": a, "lte": b}}}
        if quoi == "prefix":
            # Y compris sur un sous-champ qui n'est pas une chaine : ES refuse,
            # et la verification manquait du cote `nested` de ferrite.
            v = self._valeur_pour(c, docs)
            if not isinstance(v, str) or not v:
                return {"prefix": {c.chemin: "20"}}
            return {"prefix": {c.chemin: v[:2]}}
        if quoi == "terms":
            return {"terms": {c.chemin: [self._valeur_pour(c, docs)
                                         for _ in range(rng.randint(1, 2))]}}
        return {"term": {c.chemin: self._valeur_pour(c, docs)}}

    # -- corps de la recherche ---------------------------------------------

    def corps(self, champs, docs, nb_docs):
        """Le corps complet d'un `_search` : query, tri, pagination, _source, aggs."""
        rng = self.rng
        corps = {}
        if self.brique("corps.query"):
            corps["query"] = self.feuille(champs, docs)
        if self.brique("corps.track_total_hits"):
            # Toujours pose : sans lui, ES arrete de compter a 10 000 et rend
            # `relation: "gte"`. Le comparer serait comparer un seuil.
            corps["track_total_hits"] = True

        triables = [c for c in champs if c.ty in TRIABLES]
        tri = None
        if triables and rng.random() < 0.5 and self.brique("corps.sort"):
            n = min(len(triables), rng.randint(1, 2))
            tri = [{c.chemin: {"order": rng.choice(["asc", "desc"])}}
                   for c in rng.sample(triables, n)]
            # Une fois sur douze, la cle de tri est un sous-champ de `nested`
            # pris depuis la racine. ES refuse (« it is mandatory to set the
            # [nested] context ») ; ferrite triait sur les valeurs a plat et
            # rendait un ordre en 200. Le refus est declare
            # (`nested.tri_et_aggs`), et il faut qu'il reste exerce : une
            # correction que plus personne ne pose se defait en silence.
            feuilles = feuilles_de_nested(champs)
            if feuilles and rng.random() < 1 / 12:
                c = rng.choice(feuilles)
                tri.insert(0, {c.chemin: {"order": rng.choice(["asc", "desc"])}})
            if rng.random() < 0.3:
                tri.insert(0, "_score")
            # La cle unique en dernier : le tri devient **total**, donc une
            # pagination tronquante a une seule reponse possible. Sans elle,
            # deux moteurs ont le droit de couper un paquet d'ex aequo
            # differemment — et ce n'est pas une divergence (voir l'entete).
            tri.append({TIEBREAK: {"order": "asc"}})
            corps["sort"] = tri

        if self.brique("corps.from_size"):
            # `_score` en tete d'un tri le rend dependant du moteur : BM25 n'est
            # pas le meme des deux cotes, donc tronquer une liste qu'il ordonne
            # n'a pas de reponse unique. On ne tronque que sur un ordre total
            # fait de cles de champs.
            if tri is not None and not positions_du_score(corps | {"sort": tri}) \
                    and rng.random() < 0.6:
                corps["size"] = rng.randint(0, max(1, nb_docs // 2))
                corps["from"] = rng.randint(0, 5)
            else:
                # Sans ordre total, tout le corpus sort : l'ordre se compare
                # entier, avec la regle des ex aequo.
                corps["size"] = nb_docs + 5
        if rng.random() < 0.35 and self.brique("corps.source"):
            corps["_source"] = rng.choice([
                True, False,
                [c.nom for c in champs[:2]],
                {"includes": ["*"], "excludes": [champs[0].nom]},
                {"includes": [champs[0].nom + "*"]},
            ])
        if rng.random() < 0.3 and self.brique("corps.fields"):
            corps["fields"] = self._fields(champs)
        if rng.random() < 0.2 and self.brique("corps.docvalue_fields"):
            dv = self._docvalue(champs)
            if dv:
                corps["docvalue_fields"] = dv
        if rng.random() < 0.1 and self.brique("corps.stored_fields"):
            # Jamais `_none_` : il retire `_id`, et c'est `_id` qui apparie les
            # hits des deux serveurs. Ce cas-la est mesure par
            # `sonde_fields.py`, qui compare le hit entier sans l'apparier.
            corps["stored_fields"] = rng.choice(
                [[c.nom for c in champs[:2]], ["*"], []])
        if rng.random() < 0.4 and self.brique("corps.aggs"):
            corps["aggs"] = self.aggs(champs, docs)
        return corps

    def _fields(self, champs):
        """Ce que `fields` demande : des noms, des motifs, un `format`.

        Les sous-champs de `nested` y sont **exprès** : c'est la seule forme du
        bloc qui n'est pas plate (`{"lignes": [{"ref": [...]}, ...]}`), donc la
        seule que le reste du fuzzer ne peut pas exercer par accident."""
        rng = self.rng
        adressables = [c for c in champs if c.ty not in ("object", "nested")]
        adressables += feuilles_de_nested(champs)
        out = []
        for _ in range(rng.randint(1, 3)):
            r = rng.random()
            if r < 0.12:
                out.append("*")
            elif r < 0.2 and adressables:
                out.append(rng.choice(adressables).chemin.split(".")[0] + "*")
            elif r < 0.3:
                # Un champ que le mapping ne connait pas : ES ne rend pas de
                # cle du tout, et `include_unmapped` va le chercher dans le
                # `_source` — ce que Kibana envoie sur chaque recherche.
                out.append({"field": "*", "include_unmapped": True})
            elif adressables:
                c = rng.choice(adressables)
                if c.ty == "date" and rng.random() < 0.5:
                    out.append({"field": c.chemin,
                                "format": rng.choice(
                                    ["yyyy-MM-dd", "epoch_millis",
                                     "strict_date_optional_time"])})
                else:
                    out.append(c.chemin)
        return out or ["*"]

    def _docvalue(self, champs):
        """Ce que `docvalue_fields` demande : des colonnes.

        Un `text` n'en a pas : ES fait echouer le shard, ferrite aussi, et le
        cas sort une fois sur dix pour que ce refus reste exerce des deux
        cotes."""
        rng = self.rng
        colonnes = [c for c in champs
                    if c.ty not in ("object", "nested", "text", "text_devine")]
        textes = [c for c in champs if c.ty in ("text", "text_devine")]
        if textes and rng.random() < 0.1:
            return [rng.choice(textes).chemin]
        if not colonnes:
            return []
        out = []
        for c in rng.sample(colonnes, min(len(colonnes), rng.randint(1, 3))):
            if c.ty == "date" and rng.random() < 0.4:
                out.append({"field": c.chemin,
                            "format": rng.choice(["yyyy-MM-dd", "epoch_millis"])})
            else:
                out.append(c.chemin)
        return out

    def aggs(self, champs, docs, prof=0):
        rng = self.rng
        aggs = {}
        for i in range(rng.randint(1, 2)):
            a = self._agg(champs, docs, prof)
            if a is not None:
                aggs[f"a{prof}_{i}"] = a
        return aggs or {"a": {"value_count": {"field": "_id"}}}

    def _agg(self, champs, docs, prof):
        rng = self.rng

        # Une agregation posee sur un sous-champ de `nested` **depuis la
        # racine**. Chez ES ces valeurs vivent dans des documents caches : il
        # n'en voit aucune et rend le resultat vide de l'agregation (`null`,
        # `0.0`, `buckets: []`). ferrite les porte sur le document parent — il
        # agregeait donc a plat et rendait un autre nombre, en 200. Il refuse
        # maintenant, et le refus est declare (`nested.tri_et_aggs`).
        #
        # La brique citee est celle de l'agregation elle-meme, pas une
        # nouvelle : ce qui est exerce ici est bien une metrique ou un `terms`,
        # sur un champ qui se trouve etre sous un `nested`.
        feuilles = feuilles_de_nested(champs)
        if feuilles and rng.random() < 0.08:
            c = rng.choice(feuilles)
            if (c.ty in NUMERIQUES or c.ty == "date") and self.brique("agg.metriques"):
                nom = rng.choice(["min", "max", "value_count", "stats"]
                                 if c.ty == "date"
                                 else ["min", "max", "sum", "avg", "value_count",
                                       "stats"])
                return {nom: {"field": c.chemin}}
            if self.brique("agg.terms"):
                return {"terms": {"field": c.chemin}}

        numeriques = [c for c in champs if c.ty in NUMERIQUES]
        dates = [c for c in champs if c.ty == "date"]
        cles = [c for c in champs
                if c.ty in ("keyword", "boolean", "date") + tuple(NUMERIQUES)]
        possibles = []
        if numeriques or dates:
            possibles += ["metrique"] * 3
        if cles:
            possibles += ["terms"] * 3
        if numeriques or dates:
            possibles += ["range"]
        if numeriques:
            possibles += ["histogram"]
        if dates:
            possibles += ["date_histogram"]
        possibles += ["filter"]
        if not possibles:
            return None
        quoi = rng.choice(possibles)

        if quoi == "metrique" and self.brique("agg.metriques"):
            c = rng.choice(numeriques + dates)
            nom = rng.choice(["min", "max", "sum", "avg", "value_count", "stats"])
            if c.ty == "date" and nom in ("sum", "avg"):
                nom = "max"
            q = {"field": c.chemin}
            if rng.random() < 0.25 and c.ty in NUMERIQUES:
                q["missing"] = 0
            return {nom: q}
        if quoi == "terms" and self.brique("agg.terms"):
            c = rng.choice(cles)
            q = {"field": c.chemin}
            if rng.random() < 0.5:
                q["size"] = rng.choice([1, 3, 10, 50])
            if rng.random() < 0.35:
                q["min_doc_count"] = rng.choice([0, 1, 2, 3])
            if rng.random() < 0.3:
                q["order"] = {rng.choice(["_count", "_key"]):
                              rng.choice(["asc", "desc"])}
            return self._peut_etre_sous(q, "terms", champs, docs, prof)
        if quoi == "range" and self.brique("agg.range"):
            # Sur un champ date, une borne de `range` est une **date**, ecrite
            # au format du champ : tantivy compte en nanosecondes, donc c'est
            # tout un chemin de conversion qui se mesure ici.
            if dates and (not numeriques or rng.random() < 0.4):
                c = rng.choice(dates)
                fmt = (c.mapping or {}).get("format")
                bornes = sorted([self._date(fmt) for _ in range(3)],
                                key=str)
                q = {"field": c.chemin,
                     "ranges": [{"to": bornes[0]},
                                {"from": bornes[0], "to": bornes[1]},
                                {"from": bornes[2]}]}
            else:
                c = rng.choice(numeriques)
                bornes = sorted(rng.sample([-100, 0, 10, 100, 1000], 3))
                q = {"field": c.chemin,
                     "ranges": [{"to": bornes[0]},
                                {"from": bornes[0], "to": bornes[1]},
                                {"from": bornes[2]}]}
            if rng.random() < 0.3:
                q["keyed"] = True
            if rng.random() < 0.3:
                q["ranges"][0]["key"] = "avant"
            return self._peut_etre_sous(q, "range", champs, docs, prof)
        if quoi == "histogram" and self.brique("agg.histogram"):
            c = rng.choice(numeriques)
            q = {"field": c.chemin, "interval": rng.choice([1, 10, 100, 1000])}
            if rng.random() < 0.3:
                q["min_doc_count"] = rng.choice([0, 1])
            if rng.random() < 0.2:
                q["offset"] = 5
            return self._peut_etre_sous(q, "histogram", champs, docs, prof)
        if quoi == "date_histogram" and self.brique("agg.date_histogram"):
            c = rng.choice(dates)
            q = {"field": c.chemin,
                 "fixed_interval": rng.choice(["1d", "30d", "12h", "365d"])}
            if rng.random() < 0.3:
                q["min_doc_count"] = 1
            return self._peut_etre_sous(q, "date_histogram", champs, docs, prof)
        if quoi == "filter" and self.brique("agg.filter"):
            # `filter` sous une agregation de buckets est refusee : elle ne sort
            # qu'au premier niveau.
            if prof > 0:
                return None
            return {"filter": self.feuille(champs, docs, 1)}
        return None

    def _peut_etre_sous(self, q, nom, champs, docs, prof):
        if prof < 1 and self.rng.random() < 0.3 and self.brique("agg.sous"):
            return {nom: q, "aggs": self.aggs(champs, docs, prof + 1)}
        return {nom: q}


# ---------------------------------------------------------------------------
# Normalisation et comparaison
# ---------------------------------------------------------------------------


def ecart(ecarts, chemin, a, b, texte=None):
    """Un ecart garde ses **valeurs**, pas seulement sa phrase.

    C'est ce qui permet de decider, plus bas, si un ecart tombe dans une
    divergence assumee — la decision se prend sur les nombres, pas sur une
    expression reguliere posee sur du texte."""
    ecarts.append({
        "chemin": chemin, "a": a, "b": b,
        "texte": texte or f"{chemin} : {json.dumps(a, default=str)[:70]} / "
                          f"{json.dumps(b, default=str)[:70]}",
    })


def arbre_egal(a, b, chemin, ecarts, tol=1e-9):
    if isinstance(a, dict) and isinstance(b, dict):
        for cle in sorted(set(a) | set(b)):
            if cle not in a:
                ecart(ecarts, f"{chemin}.{cle}", None, b[cle],
                      f"{chemin}.{cle} : absent a gauche "
                      f"(droite : {json.dumps(b[cle], default=str)[:60]})")
            elif cle not in b:
                ecart(ecarts, f"{chemin}.{cle}", a[cle], None,
                      f"{chemin}.{cle} : en trop a gauche "
                      f"({json.dumps(a[cle], default=str)[:60]})")
            else:
                arbre_egal(a[cle], b[cle], f"{chemin}.{cle}", ecarts, tol)
    elif isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            ecart(ecarts, chemin, len(a), len(b),
                  f"{chemin} : {len(a)} elements a gauche, {len(b)} a droite")
        for i, (x, y) in enumerate(zip(a, b)):
            arbre_egal(x, y, f"{chemin}[{i}]", ecarts, tol)
    elif isinstance(a, bool) or isinstance(b, bool):
        if a is not b:
            ecart(ecarts, chemin, a, b)
    elif isinstance(a, float) or isinstance(b, float):
        # Deux sommes de doubles dans un ordre different ne sont pas bit-a-bit
        # egales : la comparaison est relative.
        if a is None or b is None:
            if a is not b:
                ecart(ecarts, chemin, a, b)
        elif abs(float(a) - float(b)) > tol * max(1.0, abs(float(b))):
            ecart(ecarts, chemin, a, b)
    elif a != b:
        ecart(ecarts, chemin, a, b)


# ---------------------------------------------------------------------------
# Les divergences assumees
# ---------------------------------------------------------------------------


def _f32(x):
    """La valeur d'un `float` d'ES : un flottant 32 bits."""
    return struct.unpack("f", struct.pack("f", x))[0]


def _meme_float32(e):
    """Le meme nombre, ecrit avec le nombre de decimales d'un `float`.

    ES stocke un champ `float` sur **32 bits** et l'imprime avec le plus court
    texte qui y revient (`2894.4688`) ; ferrite le traduit en `f64` et
    l'imprime entier (`2894.46875`). Les deux designent le meme flottant 32
    bits — c'est verifie ici, ce n'est pas suppose — mais le texte JSON
    differe. Seule une reecriture du stockage des `float` sur 32 bits le
    ferait disparaitre ; en attendant c'est une divergence **declaree**
    (`type.flottants` dans compat.yaml), pas un ecart tu."""
    a, b = e["a"], e["b"]
    if not isinstance(a, (int, float)) or not isinstance(b, (int, float)):
        return False
    if isinstance(a, bool) or isinstance(b, bool) or a == b:
        return False
    return _f32(a) == _f32(b)


def _instant(texte):
    """Un instant en millisecondes, lu dans un `*_as_string` d'ES.

    Deux formes sortent d'un champ date : l'ISO (`2026-01-03T00:00:00.000Z`) et
    l'epoch en chaine (`1627483113433`), selon le `format` du champ."""
    if not isinstance(texte, str):
        return None
    if texte.lstrip("-").isdigit():
        return int(texte)
    try:
        return int(datetime.datetime.fromisoformat(
            texte.replace("Z", "+00:00")).timestamp() * 1000)
    except ValueError:
        return None


def _somme_de_dates(e):
    """Deux dates que la meme tolerance separe deja sur le nombre.

    `avg` et `sum` somment des doubles ; tantivy et Lucene ne les somment pas
    dans le meme ordre, donc le resultat differe dans ses derniers bits. La
    valeur **numerique** passe la tolerance relative de 1e-9 appliquee partout
    ailleurs ; sa forme lisible, tronquee a la milliseconde par ES, tombe alors
    d'un cran a cote. La forme lisible ne peut pas etre plus stricte que le
    nombre dont elle sort : c'est la meme tolerance qui s'applique."""
    if not e["chemin"].endswith("_as_string"):
        return False
    a, b = _instant(e["a"]), _instant(e["b"])
    if a is None or b is None:
        return False
    return abs(a - b) <= max(1.0, abs(float(b))) * 1e-9


def _court_circuit(e, requete, ecarts=()):
    """ES n'a pas construit la clause fautive, ferrite l'a lue.

    Un `bool` qui ne peut rien rendre — une clause obligatoire `match_none`, ou
    un `must_not` qui prend tous les documents — ne rend rien : ES
    s'arrete la et ne construit jamais les autres clauses, donc il ne voit pas
    qu'une valeur y est illisible pour le type du champ. ferrite valide la
    requete entiere avant de l'executer — le contraire ferait dependre la
    validation de l'ordre d'evaluation.

    Le meme court-circuit se produit a l'ouverture d'un `scroll`, et le
    predicat le manquait : il ne regardait que le chemin `statut` d'une
    recherche. Un troisieme predicat trop etroit, trouve comme les deux
    premiers — par une plage de graines qu'on n'avait jamais regardee."""
    chemin = e.get("chemin", "")
    if chemin.startswith("scroll") and chemin != "scroll.statut":
        # Un scroll refuse produit plusieurs ecarts (pages, documents, motif) ;
        # c'est le statut porte par l'un d'eux qui les explique tous.
        return any(_court_circuit(x, requete) for x in ecarts
                   if x.get("chemin") == "scroll.statut")
    if chemin == "scroll.statut":
        if not (e.get("b") == 200 and e.get("a") != 200):
            return False
    elif chemin != "statut" or "droite 200" not in e["texte"]:
        return False
    # Deux facons de vider un `bool` avant d'avoir lu ses autres clauses : une
    # clause obligatoire `match_none`, ou un `must_not` qui prend tout.
    def vide(noeud):
        if isinstance(noeud, list):
            return any(vide(x) for x in noeud)
        if not isinstance(noeud, dict):
            return False
        if "match_none" in noeud:
            return True
        interdit = (noeud.get("bool") or {}).get("must_not")
        if isinstance(interdit, list) and any("match_all" in x for x in interdit
                                              if isinstance(x, dict)):
            return True
        return any(vide(v) for v in noeud.values())

    if vide(requete):
        return True
    # Le troisieme declencheur n'est pas syntaxique : une clause qui ne peut
    # correspondre a **aucun document** vide le `bool` a la reecriture, et ES
    # n'a alors jamais construit les clauses suivantes. Aucune lecture de la
    # requete ne le dit — il faut le mesurer, et c'est ce que fait le probe de
    # `Cas.jouer` : il repose la clause fautive **seule** a ES. Si ES la refuse
    # aussi quand elle est seule, son 200 prouve qu'il ne l'a pas construite ;
    # s'il l'accepte seule, ferrite est trop strict et l'ecart est reel.
    return bool(_illisible_confirme.get("court_circuite"))




def _nested_et_score(e, requete):
    """L'ordre par `_score` sous un `nested`.

    ferrite evalue la requete interne **a plat** : il n'a pas de score par
    element, la ou ES fait la moyenne des scores des elements qui ont
    correspondu (`score_mode`). Les documents rendus sont les memes ; leur
    ordre par pertinence, non. C'est la divergence declaree par
    `nested.score_mode` dans compat.yaml."""
    if e.get("chemin") != "hits.ordre":
        return False
    corps = json.dumps(requete)
    tri = requete.get("sort")
    par_score = tri is None or "_score" in json.dumps(tri)
    return '"nested"' in corps and par_score


# Rempli par `Cas.jouer` juste avant de juger une divergence : est-ce que la
# clause `exists` de cette requete rend vraiment moins de documents chez ferrite ?
# La question se **mesure**, elle ne se suppose pas.
_exists_confirme = {"ampute": False}
# Meme mecanique pour le court-circuit d'ES : la question se mesure, elle ne se
# lit pas dans la requete (voir `_court_circuit`).
_illisible_confirme = {"court_circuite": False}


def _corpus_ampute(ecarts):
    """ferrite a-t-il vu **moins** de documents que l'oracle ?"""
    for x in ecarts:
        if x["chemin"].endswith(("hits.total.value", "doc_count")) \
                and isinstance(x["a"], int) and isinstance(x["b"], int) \
                and x["a"] < x["b"]:
            return True
        # Le meme constat vu par un scroll : moins de documents deroules.
        if x["chemin"] == "scroll.documents" and isinstance(x["a"], int) \
                and isinstance(x["b"], int) and x["a"] < x["b"]:
            return True
    return False


def _exists_tous_nies(noeud, nie=False):
    """Tous les `exists` de cette requete sont-ils sous un `must_not` ?

    La question decide du **sens** de la divergence declaree : ferrite voit
    moins de documents qu'ES sur un `exists` (voir [`_exists_sur_text`]), donc
    il en voit **plus** des que la clause est niee. Un pre-filtre doit etre un
    sur-ensemble ; une negation retourne l'inegalite, elle ne l'annule pas.

    Le predicat exige que **tous** les `exists` soient nies : melanger les deux
    sens rendrait n'importe quel ecart explicable, et c'est exactement ce qu'un
    predicat ne doit pas devenir.
    """
    trouves = []

    def descendre(n, nie):
        if isinstance(n, list):
            for x in n:
                descendre(x, nie)
        elif isinstance(n, dict):
            for cle, v in n.items():
                if cle == "exists":
                    trouves.append(nie)
                else:
                    descendre(v, nie or cle == "must_not")

    descendre(noeud, nie)
    return bool(trouves) and all(trouves)


def _en_trop_a_gauche(e, ecarts):
    """L'ecart est-il « ferrite en rend **plus** », et rien d'autre ?

    Le miroir de [`_corpus_ampute`], pour une divergence dont la negation a
    retourne le signe. Tout ce qu'une reponse porte en decoule — un document de
    plus, un bucket de plus, un `max_score` non nul la ou l'autre n'a rien
    trouve — mais chaque chemin est nomme, pour la meme raison que le predicat
    d'origine : une tolerance en bloc masquerait le defaut suivant.
    """
    if e["chemin"].endswith(("hits.total.value", "doc_count")):
        return isinstance(e["a"], int) and isinstance(e["b"], int) and e["a"] > e["b"]
    if e["chemin"] in ("hits.max_score", "hits.ordre"):
        return True
    if e["chemin"] == "hits.hits":
        return len(set(e["a"])) >= len(set(e["b"])) and set(e["b"]) <= set(e["a"])
    if e["chemin"].startswith("scroll"):
        return True
    # Un compte plus grand quelque part suffit a expliquer le reste de la
    # reponse, comme dans le sens d'origine.
    return any(x["chemin"].endswith(("hits.total.value", "doc_count"))
               and isinstance(x["a"], int) and isinstance(x["b"], int)
               and x["a"] > x["b"] for x in ecarts)


def _exists_sur_text(e, requete, ecarts):
    """`exists` sur un `text` dont la valeur ne produit aucun terme.

    ES tient un `_field_names` : le champ « existe » des qu'il est present dans
    `_source`, meme si sa valeur est `""`, `"   "` ou `"!!!"`. ferrite lit
    l'index inverse, ou ces valeurs n'ont laisse aucun terme — il rend donc
    **moins** de documents. Divergence declaree (`dsl.exists` dans
    compat.yaml) : la corriger demanderait de stocker les valeurs de chaque
    champ `text` une seconde fois, en colonne.

    Le predicat est volontairement etroit : uniquement un manque **a gauche**,
    et uniquement sous une requete qui contient un `exists`. Il masquerait un
    autre defaut d'`exists` qui rendrait lui aussi moins de documents — c'est le
    prix, et c'est pour ca qu'il est ecrit ici et pas dans une liste de codes
    d'etat.

    Une exception, et elle a ete trouvee par une plage de controle : sous un
    `must_not`, la meme divergence rend **plus** de documents a gauche. Le
    document dont ES juge le champ present est exclu par ES et garde par
    ferrite. C'est le meme defaut, pas un autre — mais son signe est inverse, et
    un predicat qui ne connaissait qu'un sens le lisait comme reel. Le sens
    n'est retourne que si **tous** les `exists` sont nies, et seulement quand la
    sonde a confirme que ferrite en voit moins sur la clause seule : la
    difference se mesure, elle ne se suppose pas."""
    if '"exists"' not in json.dumps(requete):
        return False
    if _exists_tous_nies(requete) and _exists_confirme.get("ampute"):
        return _en_trop_a_gauche(e, ecarts)
    # Des qu'un ecart montre que ferrite a vu moins de documents, tout ce que la
    # meme reponse porte en decoule : un bucket de moins, une cle de bucket
    # differente, un ordre decale. Les compter separement ferait passer une seule
    # cause pour plusieurs divergences.
    if _corpus_ampute(ecarts):
        return True
    if e["chemin"] == "hits.total.value":
        return isinstance(e["a"], int) and isinstance(e["b"], int) and e["a"] < e["b"]
    if e["chemin"].startswith("scroll"):
        return True
    # Le manque peut ne se voir **que** dans l'ordre : sous un
    # `bool { should: [exists], filter: [...] }`, un document dont ES juge le
    # champ present marque 1.0 et ferrite 0.0, sans qu'aucun compte ne bouge —
    # le `filter` les garde tous les deux. C'est le meme defaut, vu par le
    # score. Verifie par le probe de `Cas.jouer`, qui repose la clause `exists`
    # seule aux deux serveurs avant de conclure.
    if e["chemin"] == "hits.ordre" and _exists_confirme.get("ampute"):
        return True
    if e["chemin"] == "hits.hits":
        # `<=` et non `<` : des que la requete tronque (`from` / `size`), les
        # deux serveurs rendent le meme nombre de documents mais pas les memes,
        # puisque ferrite en a moins a paginer. La condition reste que ferrite
        # n'en rende jamais **plus**.
        return len(set(e["a"])) <= len(set(e["b"]))
    # Le meme manque, vu par une agregation `filter` posee sur un `exists`.
    if e["chemin"].endswith(("doc_count", "hits.total.value")):
        return isinstance(e["a"], int) and isinstance(e["b"], int) and e["a"] < e["b"]
    return False


def _es_casse(e):
    """ES 8.15 echoue la ou ferrite repond.

    Un champ `date` declare `format: epoch_millis` fait planter le formatage
    d'ES des qu'une valeur sentinelle apparait (« Field EpochMillis cannot be
    printed as the value -9223372036854775808 cannot be negative according to
    the SignStyle », « Cannot format stat [max] with format […epoch_millis…] ») :
    un `sort` sur un document sans valeur, ou un `stats` sur un bucket vide,
    rendent 400 ou 500. ferrite rend 200 et une reponse correcte. Le fuzzer le
    signale, mais ce n'est pas un defaut de ferrite.

    Le cas ne se limite pas a « ferrite repond, ES casse ». Il arrive aussi que
    **les deux** refusent, pour deux raisons sans rapport : ferrite sur un de
    ses refus declares (un trou entre deux intervalles d'un `range` sur une
    date), ES sur ce bug de formatage — 400 d'un cote, 500 de l'autre. Le
    predicat porte donc sur **le message d'ES**, pas sur les codes : quand ES
    n'arrive pas a formater sa propre reponse, il n'y a pas d'oracle, et le cas
    ne mesure rien. Ce qui n'est **pas** tolere, c'est un 500 d'ES pour une
    autre raison : celui-la reste un ecart."""
    if e.get("chemin") != "statut":
        return False
    return any(m in e["texte"] for m in (
        "cannot be negative according to the SignStyle",
        "Cannot format stat",
    ))


def _ordre_par_pertinence(e, requete):
    """Un ordre par `_score` que les statistiques de BM25 separent.

    Deux causes mesurees, toutes deux declarees dans `docs/compat.md` :

    * l'`avgdl` de BM25 — Lucene le calcule sur les documents **qui ont le
      champ**, tantivy sur **tous** les documents de l'index. Des qu'un champ
      `text` est facultatif, les deux moteurs n'ont pas la meme longueur
      moyenne, et deux scores voisins peuvent s'inverser ;
    * `fuzzy`, que tantivy note a score constant la ou Lucene pondere chaque
      terme par sa distance d'edition.

    La ligne est etroite **exprès** : elle n'accepte que le cas ou ES lui-meme
    donne deux scores **differents** aux documents echanges. Si ES les classe
    ex aequo, l'inversion ne vient pas de BM25 mais d'une cle de tri — et c'est
    par la qu'ont ete trouves le `term` sur un numerique (note par BM25 au lieu
    de 1.0) et le tri sur un champ multivalue. Aucun d'eux n'aurait ete masque
    ici. La dimension elle-meme a son outil : `diff_relevance.py`, qui compare
    l'ordre sur un corpus de 600 documents."""
    if e.get("chemin") != "hits.ordre":
        return False
    tri = requete.get("sort")
    if tri is not None and "_score" not in json.dumps(tri[:1]):
        return False
    # `e["a"]` / `e["b"]` sont les deux identifiants echanges ; le texte porte
    # leurs cles telles qu'ES les rend.
    import re as _re
    cles = _re.search(r"\(cles (.*) / (.*)\)$", e["texte"])
    if not cles:
        return False
    premier = lambda s: s.strip("[]").split(",")[0].strip()  # noqa: E731
    try:
        return float(premier(cles.group(1))) != float(premier(cles.group(2)))
    except ValueError:
        return False


def _refus_declare(e, _requete=None, ecarts=()):
    """Un refus que `compat.yaml` annonce, prononce la ou ES sait repondre.

    Ce n'est pas une divergence a corriger : c'est un cout de perimetre, et
    surtout ce n'est **pas** un silence — le client recoit une erreur au format
    d'ES qui dit pourquoi. La reconnaissance se fait sur la phrase que ferrite
    prononce, pas sur le code d'etat : « 400 » tout court ne prouverait rien."""
    chemin = e.get("chemin", "")
    if chemin.startswith("scroll") and chemin != "scroll.motif":
        # Un scroll refuse produit plusieurs ecarts (statut, pages, documents) ;
        # c'est le **motif** porte par l'un d'eux qui les explique tous.
        return any(_refus_declare(x) for x in ecarts
                   if x.get("chemin") == "scroll.motif")
    if chemin not in ("statut", "scroll.motif"):
        return False
    # « la ou ES sait repondre » est la moitie qui compte : depuis que le
    # texte d'un ecart de statut porte les **deux** messages, la phrase de
    # ferrite s'y trouve meme quand ES echoue de son cote. Sans cette
    # condition, un 500 d'ES passerait pour un cout de perimetre de ferrite.
    if chemin == "statut" and "droite 200" not in e["texte"]:
        return False
    return any(m in e["texte"] for m in (
        "champ multivalue",           # histogram / range / date_histogram
        "intervalles qui se chevauchent",  # range aux bornes qui se recouvrent
        "[tie_breaker] ne s'applique",     # tie_breaker sous un `most_fields`
        "un **trou** entre deux intervalles",  # range agg sur une date
        "[min_doc_count:",                 # terms : seule sa valeur par defaut
        # Un sous-champ de `nested` pris depuis la racine — clause, tri ou
        # agregation. ES rend 0 hit / un resultat vide, ferrite le dit.
        "est sous le champ [nested]",
        "on the nested sort field",
    ))


# Chaque entree : (nom, predicat). Un predicat qui declare un second parametre
# recoit aussi la requete — certaines divergences se reconnaissent a ce qu'on a
# demande, pas a ce qui est revenu.
DIVERGENCES_ASSUMEES = [
    ("float sur 32 bits", _meme_float32),
    ("refus declare", _refus_declare),
    ("somme de dates, a la tolerance des nombres", _somme_de_dates),
    ("court-circuit d'ES", _court_circuit),
    ("ordre par score sous un nested", _nested_et_score),
    ("exists sur un text sans terme", _exists_sur_text),
    ("ES 8.15 casse sur epoch_millis", _es_casse),
    ("ordre par pertinence (BM25)", _ordre_par_pertinence),
]


def assumee(ecarts, requete=None):
    """Le nom de la divergence assumee, si **chaque** ecart en a une, ou None.

    « Chaque » est le point : un ecart reel accompagne d'un ecart assume reste
    un ecart reel. Une meme reponse peut porter deux divergences assumees
    differentes (un ordre par pertinence et un `float` a l'affichage) ; ce qui
    est interdit, c'est qu'un seul ecart reste sans explication."""
    if not ecarts:
        return None
    couvertures = []
    for e in ecarts:
        trouve = None
        for nom, predicat in DIVERGENCES_ASSUMEES:
            arite = predicat.__code__.co_argcount
            if predicat(*(e, requete, ecarts)[:arite]):
                trouve = nom
                break
        if trouve is None:
            return None
        couvertures.append(trouve)
    # Chaque ecart est justifie ; le nom rendu est celui qui revient le plus.
    return max(set(couvertures), key=couvertures.count)


def positions_du_score(corps):
    """Les positions de `_score` dans le `sort` demande.

    Quand on trie par `_score`, la valeur du score se retrouve **dans** le
    tableau `sort` de chaque hit : la neutraliser dans `_score` sans la
    neutraliser la serait comparer BM25 par la bande."""
    out = []
    for i, cle in enumerate(corps.get("sort") or []):
        nom = cle if isinstance(cle, str) else next(iter(cle))
        if nom == "_score":
            out.append(i)
    return out


def motif(r):
    """Le motif d'une erreur, en descendant jusqu'au shard qui a echoue.

    « all shards failed » ne dit rien ; la vraie phrase est un cran plus bas."""
    err = r.get("error")
    if not isinstance(err, dict):
        return ""
    for echec in err.get("failed_shards") or []:
        raison = (echec.get("reason") or {}).get("reason")
        if raison:
            return raison
    return err.get("reason", "")


def compare_recherche(st_a, ra, st_b, rb, tri_score=()):
    """Compare deux reponses de `_search`. Rend (verdict, ecarts).

    verdict : "ok" | "ecart" | "silence" | "refus"
      silence — le serveur de gauche rend 200 la ou celui de droite refuse.
                C'est le pire resultat possible de ce projet.
      refus   — l'inverse : la gauche refuse ce que la droite sait faire."""
    ecarts = []
    if st_a != st_b:
        if st_a == 200:
            ecart(ecarts, "statut", st_a, st_b,
                  f"gauche 200, droite {st_b} "
                  f"({(rb.get('error') or {}).get('type', '?')} : "
                  f"{motif(rb)[:160]})")
            return "silence", ecarts
        if st_b == 200:
            ecart(ecarts, "statut", st_a, st_b,
                  f"gauche {st_a} ({(ra.get('error') or {}).get('type', '?')} : "
                  f"{motif(ra)[:160]}), droite 200")
            return "refus", ecarts
        # Les deux refusent, mais pas pareil. Le message des deux cotes part
        # dans le texte : sans lui, « statuts 400 / 500 » ne se diagnostique
        # pas — et c'est justement ce qu'il faut pour qu'un predicat puisse
        # trancher (voir `_es_casse`).
        ecart(ecarts, "statut", st_a, st_b,
              f"statuts {st_a} / {st_b} (gauche {motif(ra)[:120]} | "
              f"droite {motif(rb)[:160]})")
        return "ecart", ecarts
    if st_a != 200:
        # Les deux refusent : seul le statut se compare (voir l'entete).
        return "ok", []

    ha, hb = ra.get("hits", {}), rb.get("hits", {})
    arbre_egal(ha.get("total"), hb.get("total"), "hits.total", ecarts)
    # `max_score` : seule sa presence se compare (null quand un tri est demande).
    if (ha.get("max_score") is None) != (hb.get("max_score") is None):
        ecart(ecarts, "hits.max_score", ha.get("max_score"), hb.get("max_score"))

    ida = [h["_id"] for h in ha.get("hits", [])]
    idb = [h["_id"] for h in hb.get("hits", [])]
    if set(ida) != set(idb):
        ecart(ecarts, "hits.hits", ida, idb,
              f"documents differents — manquants a gauche : "
              f"{sorted(set(idb) - set(ida))[:5]}, en trop : "
              f"{sorted(set(ida) - set(idb))[:5]}")
    elif ida != idb:
        # Meme regle que diff_relevance.py : une permutation ne compte pas si
        # la droite (l'oracle) donne le meme score aux deux documents echanges.
        scores = {h["_id"]: h.get("_score") for h in hb.get("hits", [])}
        cles = {}
        for h in hb.get("hits", []):
            cles[h["_id"]] = h.get("sort", scores.get(h["_id"]))
        divergents = [(x, y) for x, y in zip(ida, idb)
                      if x != y and cles.get(x) != cles.get(y)]
        if divergents:
            x, y = divergents[0]
            ecart(ecarts, "hits.ordre", x, y,
                  f"ordre — gauche place [{x}] la ou droite place [{y}] "
                  f"(cles {cles.get(x)} / {cles.get(y)})")

    # Le contenu de chaque hit, sans ce qui ne peut pas coincider.
    ca = {h["_id"]: nettoie_hit(h, tri_score) for h in ha.get("hits", [])}
    cb = {h["_id"]: nettoie_hit(h, tri_score) for h in hb.get("hits", [])}
    for cle in sorted(set(ca) & set(cb)):
        arbre_egal(ca[cle], cb[cle], f"hits[{cle}]", ecarts)

    if ("aggregations" in ra) != ("aggregations" in rb):
        ecart(ecarts, "aggregations", "aggregations" in ra, "aggregations" in rb,
              f"aggregations : {'presente' if 'aggregations' in ra else 'absente'} "
              f"a gauche, l'inverse a droite")
    else:
        arbre_egal(ra.get("aggregations"), rb.get("aggregations"),
                   "aggregations", ecarts)
    arbre_egal(ra.get("timed_out"), rb.get("timed_out"), "timed_out", ecarts)
    return ("ecart" if ecarts else "ok"), ecarts


def nettoie_hit(h, tri_score=()):
    out = {k: v for k, v in h.items()
           if k not in ("_score", "_seq_no", "_primary_term", "_version",
                        "_node", "_shard", "_ignored")}
    if tri_score and isinstance(out.get("sort"), list):
        out["sort"] = [v for i, v in enumerate(out["sort"]) if i not in tri_score]
    return out


def types_du_mapping(m, prefixe=""):
    """Le mapping reduit a `chemin -> type`.

    ES rend des parametres que ferrite ne rend pas (et l'inverse) ; ce qui
    change les **resultats**, c'est le type. C'est donc lui qu'on compare."""
    out = {}
    for nom, champ in (m or {}).items():
        chemin = f"{prefixe}{nom}"
        if "properties" in champ:
            out[chemin] = champ.get("type", "object")
            out.update(types_du_mapping(champ["properties"], chemin + "."))
        else:
            out[chemin] = champ.get("type")
        for sous, sm in (champ.get("fields") or {}).items():
            out[f"{chemin}.{sous}"] = sm.get("type")
    return out


# ---------------------------------------------------------------------------
# Un cas
# ---------------------------------------------------------------------------

NB_DOCS = 25
NB_REQUETES = 12


class Cas:
    def __init__(self, graine, perimetre, gauche, droite, noms, bavard=False):
        self.graine = graine
        self.gen = Generateur(graine, perimetre)
        self.p = perimetre
        self.serveurs = [gauche, droite]
        self.noms = noms
        self.bavard = bavard
        self.divergences = []
        self.requetes = 0

    def _dit(self, quoi, obj=None):
        if self.bavard:
            print(f"  {quoi}" + (f"\n{json.dumps(obj, indent=2, default=str)}"
                                 if obj is not None else ""))

    def divergence(self, verdict, etape, ecarts, requete=None):
        """Un ecart trouve. `ecarts` est une liste de dicts ou de phrases."""
        detail = [e["texte"] if isinstance(e, dict) else e for e in ecarts]
        d = {"graine": self.graine, "verdict": verdict, "etape": etape,
             "detail": detail[:6], "requete": requete}
        nom = assumee([e for e in ecarts if isinstance(e, dict)], requete) \
            if all(isinstance(e, dict) for e in ecarts) else None
        if nom:
            d["verdict"] = "assume"
            d["assumee"] = nom
        self.divergences.append(d)

    def jouer(self):
        gen, rng = self.gen, self.gen.rng
        props, champs = gen.mapping()
        docs = gen.documents(champs, NB_DOCS)
        self._dit("mapping", {"properties": props})

        # 1. creation de l'index — un mapping refuse d'un cote seulement est
        #    deja une divergence, et rend la suite du cas inutilisable.
        #
        #    Une fois sur quatre, le mapping n'est pas pose sur l'index mais
        #    dans un **template**, et l'index nait de l'ecriture de l'etape
        #    suivante. Le mapping compare a l'etape 3 mesure alors ce qu'un
        #    template applique vraiment, sur un mapping que personne n'a choisi.
        corps_index = {"mappings": {"properties": props},
                       "settings": {"number_of_shards": 1, "number_of_replicas": 0,
                                    **gen.reglages}}
        par_template = (gen.p.jouable("index.templates")
                        and rng.random() < 0.25)
        reps = []
        if par_template:
            gen.brique("route.template")
            corps_tpl = {"index_patterns": [INDEX], "priority": 500,
                         "template": corps_index}
            for base in self.serveurs:
                http(base, "DELETE", f"/{INDEX}")
                reps.append(http(base, "PUT", f"/_index_template/{TEMPLATE}",
                                 corps_tpl))
        else:
            gen.brique("route.creer")
            for base in self.serveurs:
                http(base, "DELETE", f"/{INDEX}")
                reps.append(http(base, "PUT", f"/{INDEX}", corps_index))
        if reps[0][0] != reps[1][0]:
            self.divergence("ecart", "template" if par_template else "creation",
                            [f"{self.noms[0]} {reps[0][0]} / {self.noms[1]} "
                             f"{reps[1][0]} — {json.dumps(reps[0][1])[:200]} | "
                             f"{json.dumps(reps[1][1])[:200]}"],
                            {"PUT /" + INDEX: corps_index})
            return self.nettoyer()
        if reps[0][0] != 200:
            return self.nettoyer()          # les deux refusent : rien a comparer

        # 2. indexation — meme constat : deux corpus differents rendraient
        #    toutes les requetes suivantes incomparables (la cascade que ce
        #    depot a deja payee trois fois).
        gen.brique("route.bulk")
        gen.brique("route.refresh")
        lignes = []
        for doc_id, doc in docs:
            lignes.append(json.dumps({"index": {"_index": INDEX, "_id": doc_id}}))
            lignes.append(json.dumps(doc))
        charge = ("\n".join(lignes) + "\n").encode()
        etats = []
        for base in self.serveurs:
            st, r = http(base, "POST", "/_bulk?refresh=true", brut=charge)
            etats.append({it["index"]["_id"]: it["index"]["status"]
                          for it in r.get("items", [])} if st == 200 else
                         {"le _bulk lui-meme": f"{st} {json.dumps(r)[:120]}"})
        if etats[0] != etats[1]:
            differents = sorted(k for k in set(etats[0]) | set(etats[1])
                                if etats[0].get(k) != etats[1].get(k))
            self.divergence(
                "ecart", "indexation",
                [f"[{k}] {self.noms[0]}={etats[0].get(k)} "
                 f"{self.noms[1]}={etats[1].get(k)} : "
                 f"{json.dumps(dict(docs).get(k))[:160]}" for k in differents[:6]],
                {"mappings": {"properties": props},
                 "documents": {k: dict(docs).get(k) for k in differents[:3]}})
            return self.nettoyer()

        # 3. le mapping obtenu — c'est la que le mapping dynamique se mesure.
        gen.brique("route.mapping")
        vus = []
        for base in self.serveurs:
            st, r = http(base, "GET", f"/{INDEX}/_mapping")
            vus.append(types_du_mapping(
                ((r.get(INDEX) or {}).get("mappings") or {}).get("properties")))
        ecarts = []
        arbre_egal(vus[0], vus[1], "mapping", ecarts)
        if ecarts:
            self.divergence("ecart", "mapping", ecarts,
                            {"mappings": {"properties": props},
                             "documents": [d for _, d in docs[:3]]})

        # 3 bis. la **description** du meme mapping. `_field_caps` n'invente
        #    rien : tout est deja dans le mapping. C'est justement pour ca
        #    qu'un mapping tire au sort la met a l'epreuve — objets imbriques,
        #    `nested`, multi-fields, champs devines par l'indexation.
        self.decrire(props)

        # 4. le compte, avant toute requete : si les corpus different, tout le
        #    reste ment.
        gen.brique("route.count")
        comptes = [http(b, "GET", f"/{INDEX}/_count")[1].get("count")
                   for b in self.serveurs]
        if comptes[0] != comptes[1]:
            self.divergence("ecart", "count",
                            [f"{self.noms[0]}={comptes[0]} / "
                             f"{self.noms[1]}={comptes[1]}"])
            return self.nettoyer()

        # 5. les requetes
        gen.brique("route.search")
        acceptes = sum(1 for s in etats[0].values() if s in (200, 201))
        for _ in range(NB_REQUETES):
            corps = gen.corps(champs, docs, acceptes)
            self.requetes += 1
            reps = [http(b, "POST", f"/{INDEX}/_search", corps)
                    for b in self.serveurs]
            self._dit("requete", corps)
            self.valider(corps, reps[0][0])
            verdict, ecarts = compare_recherche(*reps[0], *reps[1],
                                                positions_du_score(corps))
            if verdict != "ok":
                _exists_confirme["ampute"] = self.exists_ampute(corps)
                _illisible_confirme["court_circuite"] = \
                    self.illisible_court_circuitee(corps, reps)
                self.divergence(verdict, "recherche", ecarts, corps)
                if self.bavard:
                    for nom, (st, r) in zip(self.noms, reps):
                        print(f"  -- {nom} ({st})\n"
                              f"{json.dumps(r, indent=2, default=str)[:3000]}")

        # 6. le meme resultat, mais **exporte** — un `scroll` doit rendre
        #    exactement les memes documents, une fois chacun, sur ses pages.
        if rng.random() < 0.35:
            self.scroll(champs, docs)

        # 7. et enfin, la seule etape qui **ecrit**. Elle vient en dernier parce
        #    qu'elle change le corpus : tout ce qui precede l'aurait alors
        #    compare sur deux etats differents.
        if rng.random() < 0.35:
            self.par_requete(champs, docs)
        return self.nettoyer()

    def par_requete(self, champs, docs):
        """`_delete_by_query` / `_update_by_query` : les compteurs **et** l'etat.

        Deux choses se comparent ici, et la seconde est la vraie :

        * la reponse — `total`, `deleted`/`updated`, `batches`,
          `version_conflicts`. Trois valeurs en sont retirees, chacune pour une
          raison : `took` (une duree), `throttled_millis` et
          `throttled_until_millis` (une regulation qu'aucun des deux ne fait, et
          dont le compteur d'ES bouge quand une seconde de mur passe) ;
        * ce qui **reste** dans l'index, identifiant par identifiant, avec sa
          `_version` (relue par `_mget`, la seule route qui la rende sans
          parametre de recherche). Une commande qui rend les bons compteurs en
          supprimant les mauvais documents serait verte sur les compteurs seuls
          — et c'est precisement le genre d'echec silencieux que ce depot
          chasse. La `_version` est le **seul** effet observable d'un
          `_update_by_query` sans script : sans elle, une commande qui ne ferait
          rien passerait pour une commande qui reindexe tout.

        `max_docs` est tire au sort : il decide **quels** documents partent,
        donc il exerce l'ordre du balayage (`_doc`), pas seulement le compte.
        C'est lui qui a sorti le premier defaut de cette brique : ferrite
        balayait dans l'ordre des numeros de document de tantivy, qui **n'est
        pas** l'ordre d'ecriture.

        Deux garde-fous, sans lesquels cette brique mesurerait autre chose que
        ce qu'elle croit :

        * la **meme requete est d'abord posee en recherche** aux deux serveurs.
          Si elle n'y trouve pas les memes documents, l'ecart est celui du Query
          DSL — l'etape 5 le mesure deja, avec ses predicats. Comparer ce que la
          commande a supprime reviendrait alors a compter deux fois la meme
          divergence, sous un nom qui ne la designe pas ;
        * le **motif** d'un refus n'est pas compare, seulement son statut :
          ferrite nomme ses refus avec ses propres mots, expres. C'est la meme
          regle que pour le `scroll`.
        """
        gen, rng = self.gen, self.gen.rng
        supprime = rng.random() < 0.5
        if not gen.brique("route.delete_by_query" if supprime
                          else "route.update_by_query"):
            return
        route = "_delete_by_query" if supprime else "_update_by_query"
        corps = {"query": gen.feuille(champs, docs)}
        params = []
        if rng.random() < 0.4:
            corps["max_docs"] = rng.randint(1, NB_DOCS)
        if rng.random() < 0.3:
            params.append(f"scroll_size={rng.choice([1, 2, 3, 50])}")
        if rng.random() < 0.2:
            params.append("conflicts=proceed")
        chemin = f"/{INDEX}/{route}?refresh=true" + "".join("&" + p for p in params)

        # Le garde-fou : la meme requete, en recherche, avant d'ecrire.
        vises = []
        for base in self.serveurs:
            st, r = http(base, "POST", f"/{INDEX}/_search",
                         {"query": corps["query"], "size": 100, "_source": False,
                          "sort": [{TIEBREAK: {"order": "asc"}}]})
            self.requetes += 1
            vises.append(sorted(h["_id"] for h in (r.get("hits", {}).get("hits") or []))
                         if st == 200 else None)
        if vises[0] != vises[1]:
            return          # divergence du Query DSL : c'est l'etape 5 qui la dit

        self.requetes += 1
        vus, motifs = [], []
        for base in self.serveurs:
            st, r = http(base, "POST", chemin, corps)
            refuse = not isinstance(r, dict) or "error" in r
            motifs.append(motif(r) if refuse else "")
            compteurs = "refus" if refuse else {
                c: v for c, v in r.items()
                if c not in ("took", "throttled_millis", "throttled_until_millis")}
            _, reste = http(base, "POST", f"/{INDEX}/_search",
                            {"size": 100, "sort": [{TIEBREAK: {"order": "asc"}}]})
            self.requetes += 1
            ids = [h["_id"] for h in (reste.get("hits", {}).get("hits") or [])]
            versions = {}
            if ids:
                _, lus = http(base, "POST", f"/{INDEX}/_mget", {"ids": ids})
                self.requetes += 1
                versions = {d["_id"]: d.get("_version")
                            for d in (lus.get("docs") or [])}
            vus.append({
                "statut": st,
                "reponse": compteurs,
                "restants": [(i, versions.get(i)) for i in ids],
            })
        ecarts = []
        arbre_egal(vus[0], vus[1], route, ecarts)
        if ecarts:
            if any(motifs):
                ecart(ecarts, f"{route}.motif", motifs[0], motifs[1],
                      f"{route}.motif (non compare) : {motifs[0][:80]} / "
                      f"{motifs[1][:80]}")
            self.divergence("ecart", route, ecarts, {"POST " + chemin: corps})

    def decrire(self, props):
        """`_field_caps` et `_stats` sur l'index qu'on vient de remplir.

        Deux predicats ecrits, et ils sont le contenu de cette comparaison :

        * les champs de **metadonnees** (`_id`, `_index`, `_seq_no`…) sortent
          de la comparaison. ES les rend sur `fields=*`, ferrite non, et c'est
          declare : il ne sait pas les interroger, les annoncer `searchable`
          serait un resultat faux. Tout le reste — le type, `searchable`,
          `aggregatable`, la liste `indices` — se compare a l'octet pres ;
        * de `_stats`, seul `docs.count` se compare. `store.size_in_bytes`
          mesure deux moteurs de stockage differents : le comparer ne dirait
          rien de la compatibilite."""
        gen = self.gen
        if gen.p.jouable("recherche.field_caps"):
            gen.brique("route.field_caps")
            vus = []
            for base in self.serveurs:
                _, r = http(base, "GET", f"/{INDEX}/_field_caps?fields=*")
                self.requetes += 1
                champs = r.get("fields") or {}
                vus.append({nom: cap for nom, cap in champs.items()
                            if not nom.startswith("_")})
            ecarts = []
            arbre_egal(vus[0], vus[1], "field_caps", ecarts)
            if ecarts:
                self.divergence("ecart", "field_caps", ecarts,
                                {"mappings": {"properties": props}})
        if gen.p.jouable("index.stats"):
            gen.brique("route.stats")
            comptes = []
            for base in self.serveurs:
                _, r = http(base, "GET", f"/{INDEX}/_stats/docs")
                self.requetes += 1
                comptes.append(
                    (((r.get("_all") or {}).get("primaries") or {})
                     .get("docs") or {}).get("count"))
            if comptes[0] != comptes[1] and not self.nested_gonfle(props, comptes):
                self.divergence("ecart", "stats",
                                [f"_all.primaries.docs.count : "
                                 f"{self.noms[0]}={comptes[0]} / "
                                 f"{self.noms[1]}={comptes[1]}"])

    def nested_gonfle(self, props, comptes):
        """L'ecart de `docs.count` s'explique-t-il par les sous-documents
        `nested` d'ES ?

        Lucene indexe **chaque element** d'un tableau `nested` comme un document
        a part : `docs.count` d'ES les compte, et depasse donc le nombre de
        documents qu'on a envoyes. ferrite n'a pas de jointure de bloc (voir
        docs/nested-join.md) : il n'a pas ces sous-documents, et compte ce qu'il
        a. Aucun des deux ne ment.

        Le predicat n'est pas « il y a du nested, on tolere ». Il se **mesure** :
        le compte de ferrite doit egaler ce que la recherche rend des deux
        cotes, et celui d'ES doit lui etre strictement superieur. Un ecart d'une
        autre nature ressort donc quand meme."""
        if not any(t == "nested" for t in types_du_mapping(props).values()):
            return False
        cherches = []
        for base in self.serveurs:
            _, r = http(base, "GET", f"/{INDEX}/_count")
            self.requetes += 1
            cherches.append(r.get("count"))
        return (cherches[0] == cherches[1] == comptes[0]
                and comptes[1] > comptes[0])

    def valider(self, corps, statut_recherche):
        """La meme requete, posee a `_validate/query` — sans l'executer.

        Deux predicats ecrits :

        * seul `valid` se compare. L'`explanation` d'ES est la chaine Lucene de
          la requete reecrite, celle de ferrite le rendu de la requete tantivy :
          les deux moteurs ne construisent pas les memes objets, ce qui doit
          coincider est le **verdict** ;
        * un `valid: false` la ou ES dit `true` n'est un ecart **de cette
          route** que si ferrite accepte pourtant la meme requete en recherche.
          Sinon, c'est un refus que la comparaison de recherche vient deja de
          mesurer, vu depuis une autre route — le compter deux fois gonflerait
          le nombre de divergences sans rien apprendre. Ce que ce predicat
          verifie vraiment, c'est que `_validate/query` dit la **meme chose** que
          `_search` : une route qui declarerait valide ce que la recherche
          refuse serait le vrai defaut."""
        if "query" not in corps or not self.gen.p.jouable("recherche.validate_query"):
            return
        self.gen.brique("route.validate")
        verdicts = []
        for base in self.serveurs:
            st, r = http(base, "POST", f"/{INDEX}/_validate/query",
                         {"query": corps["query"]})
            self.requetes += 1
            verdicts.append(r.get("valid") if st == 200 else f"HTTP {st}")
        if verdicts[0] == verdicts[1]:
            return
        # `_validate` et `_search` s'accordent : l'ecart est celui, deja
        # mesure, de la recherche elle-meme.
        if verdicts[0] is False and statut_recherche != 200:
            return
        self.divergence("ecart", "validate_query",
                        [f"valid : {self.noms[0]}={verdicts[0]} / "
                         f"{self.noms[1]}={verdicts[1]}"],
                        {"query": corps["query"]})

    # Les clauses qui nomment un champ et lisent une valeur : ce sont elles qui
    # peuvent porter une valeur illisible pour le type du champ.
    CLAUSES_A_VALEUR = ("match", "match_phrase", "match_phrase_prefix", "term",
                        "terms", "range", "prefix", "wildcard", "regexp",
                        "fuzzy")

    def illisible_court_circuitee(self, corps, reps):
        """ferrite refuse une valeur illisible qu'ES n'a jamais lue — ou pas.

        ES construit ses clauses dans l'ordre et **s'arrete** des qu'une d'elles
        vide le `bool` : il ne voit alors jamais que la valeur d'une clause
        suivante est illisible pour le type du champ. ferrite valide la requete
        entiere avant de l'executer (le contraire ferait dependre la validation
        de l'ordre d'evaluation), donc il refuse.

        Rien dans la requete ne dit qu'ES s'est arrete : deux des declencheurs
        sont syntaxiques (`match_none`, `must_not: match_all`), le troisieme ne
        l'est pas — une clause qui ne correspond a aucun document est videe a la
        **reecriture**. La question se mesure donc : la clause fautive est
        reposee **seule** a ES.

        * ES la refuse seule -> il ne sait pas lire cette valeur non plus, donc
          son 200 sur la requete complete prouve qu'il ne l'a pas construite :
          divergence assumee ;
        * ES l'accepte seule -> ferrite est plus strict qu'ES sur cette valeur,
          et l'ecart est **reel**. Le predicat ne le masque pas.
        """
        (st_a, ra), (st_b, _) = reps[0], reps[1]
        if st_a == 200 or st_b != 200:
            return False
        phrase = motif(ra)
        champ = re.search(r"parse (?:date )?field \[([^\]]+)\]", phrase)
        if not champ:
            return False
        champ = champ.group(1)

        clauses = []

        def cueille(noeud):
            if isinstance(noeud, list):
                for x in noeud:
                    cueille(x)
            elif isinstance(noeud, dict):
                for nom, params in noeud.items():
                    if nom in self.CLAUSES_A_VALEUR and isinstance(params, dict) \
                            and champ in params:
                        clauses.append({nom: {champ: params[champ]}})
                    cueille(params)

        cueille(corps.get("query"))
        for clause in clauses:
            st, _ = http(self.serveurs[1], "POST", f"/{INDEX}/_search",
                         {"query": clause, "size": 0})
            self.requetes += 1
            if st >= 400:
                return True
        return False

    def exists_ampute(self, corps):
        """Une clause `exists` de cette requete rend-elle moins de documents ?

        La question se **mesure** : chaque `exists` de la requete est reposee
        seule aux deux serveurs. Si ferrite en rend strictement moins, l'ecart
        constate en decoule — c'est la divergence declaree sur `exists` d'un
        `text` sans terme. Sinon, l'ecart est reel et le reste."""
        champs_exists = []

        def cueille(noeud):
            if isinstance(noeud, list):
                for x in noeud:
                    cueille(x)
            elif isinstance(noeud, dict):
                q = noeud.get("exists")
                if isinstance(q, dict) and isinstance(q.get("field"), str):
                    champs_exists.append(q["field"])
                for v in noeud.values():
                    cueille(v)

        cueille(corps.get("query"))
        cueille(corps.get("aggs"))
        for champ in champs_exists:
            seule = {"query": {"exists": {"field": champ}}, "size": 0,
                     "track_total_hits": True}
            comptes = []
            for base in self.serveurs:
                st, r = http(base, "POST", f"/{INDEX}/_search", seule)
                self.requetes += 1
                comptes.append(r.get("hits", {}).get("total", {}).get("value")
                               if st == 200 else None)
            if None not in comptes and comptes[0] < comptes[1]:
                return True
        return False

    def scroll(self, champs, docs):
        """Le deroule complet d'un scroll, page par page, des deux cotes.

        Trie sur la cle unique : sans ordre total, deux moteurs ont le droit de
        couper leurs pages autrement, et ce n'est pas une divergence."""
        gen = self.gen
        if not gen.brique("corps.scroll"):
            return
        corps = {"query": gen.feuille(champs, docs),
                 "sort": [{TIEBREAK: {"order": "asc"}}],
                 "size": gen.rng.choice([1, 3, 7])}
        self.requetes += 1
        deroules, motifs = [], []
        for base in self.serveurs:
            st, r = http(base, "POST", f"/{INDEX}/_search?scroll=1m", corps)
            if st != 200:
                deroules.append({"statut": st})
                motifs.append(motif(r))
                continue
            motifs.append("")
            pages, vus = [], []
            while r["hits"]["hits"]:
                pages.append(len(r["hits"]["hits"]))
                vus.extend(h["_id"] for h in r["hits"]["hits"])
                st, r = http(base, "POST", "/_search/scroll",
                             {"scroll": "1m", "scroll_id": r["_scroll_id"]})
                if st != 200:
                    break
            http(base, "DELETE", "/_search/scroll",
                 {"scroll_id": r.get("_scroll_id")} if r.get("_scroll_id") else None)
            deroules.append({"statut": 200, "pages": pages, "documents": vus})
        ecarts = []
        arbre_egal(deroules[0], deroules[1], "scroll", ecarts)
        if ecarts:
            # Le **motif** d'un refus n'est jamais compare — ferrite nomme ses
            # refus avec ses propres mots, expres. Il est joint a l'ecart pour
            # que la ligne des refus declares puisse le reconnaitre, et pour
            # qu'un lecteur voie pourquoi le scroll s'est arrete.
            if any(motifs):
                ecart(ecarts, "scroll.motif", motifs[0], motifs[1],
                      f"scroll.motif (non compare) : {motifs[0][:80]} / "
                      f"{motifs[1][:80]}")
            self.divergence("ecart", "scroll", ecarts, corps)

    def nettoyer(self):
        self.gen.brique("route.supprimer")
        for base in self.serveurs:
            http(base, "DELETE", f"/{INDEX}")
            # Un template survit a la suppression des index : le laisser
            # derriere soi s'appliquerait aux cas suivants. C'est exactement la
            # fuite d'etat qui a deja coute cher a ce depot (voir CLAUDE.md).
            http(base, "DELETE", f"/_index_template/{TEMPLATE}")
        return self.divergences


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------


def verifie_les_briques(perimetre):
    """Toutes les briques citent-elles une capacite que compat.yaml declare ?"""
    for nom, cid in sorted(BRIQUES.items()):
        perimetre.connu(cid)
    return True


def rapport_couverture(perimetre):
    exercees, jamais = perimetre.couverture(set(BRIQUES.values()))
    print(f"# couverture : {len(exercees)} capacites exercees par le fuzzer, "
          f"{len(jamais)} declarees tenues et jamais exercees")
    print("#\n# exercees :")
    for cid in exercees:
        print(f"#   {cid}")
    print("#\n# jamais exercees (declarees supporte/partiel) — ce que ce fuzzer "
          "ne mesure pas :")
    for cid in jamais:
        print(f"#   {cid}")


def main():
    ap = argparse.ArgumentParser(add_help=True)
    ap.add_argument("urls", nargs="*")
    ap.add_argument("--cas", type=int, default=50)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--calibrer", action="store_true")
    ap.add_argument("--rejouer", type=int)
    ap.add_argument("--couverture", action="store_true")
    ap.add_argument("--json")
    ap.add_argument("--arret", action="store_true",
                    help="s'arrete a la premiere divergence")
    ap.add_argument("--tout", action="store_true",
                    help="imprime aussi les divergences assumees")
    a = ap.parse_args()

    perimetre = Perimetre()
    verifie_les_briques(perimetre)

    if a.couverture:
        # La couverture se lit sur ce que les briques declarent, pas sur ce
        # qu'un tirage a sorti : un tirage court ne prouve rien.
        rapport_couverture(perimetre)
        return 0

    defauts = (["http://localhost:9201", "http://localhost:9202"] if a.calibrer
               else ["http://localhost:9200", "http://localhost:9201"])
    urls = (a.urls + defauts[len(a.urls):])[:2]
    noms = ["ES(A)", "ES(B)"] if a.calibrer else ["ferrite", "ES"]

    for nom, base in zip(noms, urls):
        try:
            st, r = http(base, "GET", "/")
        except Exception as exc:  # noqa: BLE001
            print(f"# {nom} indisponible ({base}) : {exc}")
            return 2
        print(f"# {nom:<7} {base}  {r.get('version', {}).get('number', '?')}")

    if a.calibrer:
        print("# etalonnage : la meme batterie contre deux Elasticsearch. Tant "
              "qu'elle n'est\n#   pas a zero, ce que le fuzzer dit de ferrite "
              "ne vaut rien.")

    graines = [a.rejouer] if a.rejouer is not None else \
        list(range(a.seed, a.seed + a.cas))
    toutes, requetes = [], 0
    for i, graine in enumerate(graines):
        cas = Cas(graine, perimetre, urls[0], urls[1], noms,
                  bavard=a.rejouer is not None)
        divs = cas.jouer()
        requetes += cas.requetes
        toutes.extend(divs)
        for d in divs:
            if d["verdict"] == "assume" and not a.tout:
                continue
            print(f"[{d['verdict']:8}] graine {d['graine']} — {d['etape']}"
                  + (f" ({d['assumee']})" if d.get("assumee") else ""))
            for x in d["detail"]:
                print(f"           {x}")
            if d.get("requete") is not None:
                print(f"           requete : {json.dumps(d['requete'], default=str)[:400]}")
            print(f"           rejouer : python3 tests/compat/fuzz_vs_es.py "
                  f"{'--calibrer ' if a.calibrer else ''}--rejouer {d['graine']}")
        if a.arret and any(d["verdict"] != "assume" for d in divs):
            break
        if (i + 1) % 25 == 0:
            reels = sum(1 for d in toutes if d["verdict"] != "assume")
            print(f"# {i + 1}/{len(graines)} cas, {requetes} requetes, "
                  f"{reels} divergences")

    reelles = [d for d in toutes if d["verdict"] != "assume"]
    assumees = [d for d in toutes if d["verdict"] == "assume"]
    silences = sum(1 for d in reelles if d["verdict"] == "silence")
    print(f"\n{len(graines)} cas, {requetes} requetes generees, "
          f"{len(reelles)} divergences")
    if silences:
        print(f"  dont {silences} rendues en silence "
              f"({noms[0]} repond 200 la ou {noms[1]} refuse)")
    for nom, _ in DIVERGENCES_ASSUMEES:
        n = sum(1 for d in assumees if d.get("assumee") == nom)
        if n:
            print(f"  {n} divergences assumees « {nom} » "
                  f"(declarees dans docs/compat.md, --tout pour les voir)")
    if a.calibrer and toutes:
        print("  l'etalonnage n'est pas vert : le generateur ou la "
              "normalisation est en cause, pas ferrite")
    if a.json:
        # Les divergences **reelles** sont ecrites entierement : ce sont elles
        # qu'on relit. Les assumees sont resumees — leur nombre et trois
        # exemples par famille suffisent, et la liste complete pesait un
        # megaoctet pour ne rien apprendre de plus.
        resume = {}
        for nom, _ in DIVERGENCES_ASSUMEES:
            lot = [d for d in assumees if d.get("assumee") == nom]
            if lot:
                resume[nom] = {"compte": len(lot), "exemples": lot[:3]}
        with open(a.json, "w") as f:
            json.dump({
                "cible": noms, "graine_debut": a.seed, "cas": len(graines),
                "requetes": requetes,
                "divergences": reelles, "assumees": resume,
                "neutralisations": NEUTRALISATIONS,
            }, f, indent=2, default=str, sort_keys=True)
            f.write("\n")
        print(f"  rapport : {a.json}")
    return 1 if reelles else 0


if __name__ == "__main__":
    sys.exit(main())
