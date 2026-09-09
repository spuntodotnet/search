#!/usr/bin/env python3
"""Ponderer la couverture par l'usage reel : ce que le corpus de requetes
exerce, ce que ferrite en sert, et ce qui manque **par frequence**.

    python3 tests/compat/ponderation.py                        # la distribution
    python3 tests/compat/ponderation.py --json docs/usage.json # le rapport machine
    python3 tests/compat/ponderation.py --rejoue http://127.0.0.1:9200 http://127.0.0.1:9201
    python3 tests/compat/ponderation.py --poids                # ecrit compat.yaml
    python3 tests/compat/ponderation.py --verifie              # ce que lance la CI

# Pourquoi

« 9,7 % des cas de la suite REST » n'est pas « 9,7 % de ce qu'on m'envoie ».
Le denominateur de la conformance est un catalogue de tests, ou `bool` + `match`
pese autant qu'un `significant_terms` avec script. Cette mesure-ci change de
denominateur : des **requetes que quelqu'un a ecrites** ([`recolte_usage.py`](recolte_usage.py)),
et la question « celle-ci passe-t-elle **entierement** ? ». Une requete a 90 %
supportee est une requete qui echoue : le verdict est donc par requete, jamais
par clause.

# Trois mesures, pas une

  distribution  ce que le corpus contient : chaque clause, chaque parametre de
                corps, chaque agregation, chaque route, avec sa frequence
  croisement    le verdict **declare** : chaque trait est rattache a une
                capacite de `compat.yaml`, et une requete est servie si aucun
                de ses traits n'est refuse. Un trait qu'aucune capacite ne
                reclame compte **contre** nous — comme dans le rapport de
                conformance, sinon oublier de declarer ferait monter le taux
  rejeu         le verdict **mesure** : la meme requete posee a ferrite et a un
                vrai ES 8.15, sur un index vide des deux cotes. C'est ce qui
                etalonne le croisement — un ecart entre les deux est un defaut
                de `compat.yaml`, et il est publie comme tel

Le rejeu porte sur la **validation du corps**, pas sur les documents rendus :
les deux serveurs sont vides, on ne compare donc que « accepte » / « refuse ».
C'est exactement la question posee — est-ce que cette requete-la passerait.

# Les poids

`poids` dans [`compat.yaml`](../../compat.yaml) = la part des requetes du corpus
qui exercent la capacite, en pour-cent a une decimale. Il n'est jamais ecrit a
la main : `--poids` l'ecrit depuis cette mesure, `--verifie` echoue si les deux
divergent. Une capacite qu'aucun trait du corpus ne sait exercer garde `null` —
« pas mesurable ici » n'est pas « jamais utilise », et les confondre serait
exactement le genre de chiffre flatteur que ce depot refuse.
"""
import argparse
import collections
import json
import os
import re
import sys
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import genere_compat  # noqa: E402
import perimetre as mod_perimetre  # noqa: E402

RACINE = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
CORPUS = os.path.join(RACINE, "tests", "compat", "usage", "corpus.jsonl")
SOURCES = os.path.join(RACINE, "tests", "compat", "usage", "sources.json")
VERDICTS = os.path.join(RACINE, "tests", "compat", "usage", "verdicts.jsonl")
COMPAT = os.path.join(RACINE, "compat.yaml")

SERVI, REFUSE, INDETERMINE = "servie", "refusee", "indeterminee"


# ===========================================================================
# 1. Les traits d'une requete
# ===========================================================================

# Les clauses du Query DSL qui portent un champ : `{"match": {"titre": ...}}`.
# Il faut le savoir pour descendre d'un cran de plus et voir les parametres
# (`fuzziness`, `slop`…) sans prendre un nom de champ pour un parametre.
CLAUSES_A_CHAMP = {
    "match", "match_phrase", "match_phrase_prefix", "match_bool_prefix", "term",
    "prefix", "wildcard", "regexp", "fuzzy", "range", "span_term", "intervals",
    "terms_set", "rank_feature", "distance_feature", "geo_distance", "geo_bounding_box",
    "geo_shape", "shape", "sparse_vector", "semantic", "text_expansion",
    "weighted_tokens", "knn",
}
# Ou une clause en contient d'autres. Une requete est un arbre : le trait
# `dsl:match` doit se lever aussi quand le `match` est au fond d'un `bool`.
CONTENEURS = {
    "bool": ("must", "should", "filter", "must_not"),
    "constant_score": ("filter",),
    "dis_max": ("queries",),
    "boosting": ("positive", "negative"),
    "function_score": ("query", "filter"),
    "nested": ("query",),
    "has_child": ("query",), "has_parent": ("query",),
    "span_near": ("clauses",), "span_or": ("clauses",), "span_not": ("include", "exclude"),
    "span_first": ("match",), "span_containing": ("little", "big"),
    "span_within": ("little", "big"), "field_masking_span": ("query",),
    "percolate": (), "pinned": ("organic",), "script_score": ("query",),
}

# Ce qui porte un corps de `_search` complet, et ce qui n'en porte que la
# requete. `_delete_by_query` prend bien un `query`, mais son `conflicts` n'est
# pas un parametre de recherche : le compter comme tel inventerait un trait.
FAMILLE_RECHERCHE = {"search", "count", "msearch", "explain", "indices.validate_query"}
FAMILLE_QUERY_SEULE = {"delete_by_query", "update_by_query", "reindex"}
# Un `terms` dont la valeur est un objet lit sa liste dans un autre document.
TERMS_LOOKUP = {"index", "id", "path", "routing"}

CORPS_AGGS = ("aggs", "aggregations")
MOTS_AGG = {"meta", "aggs", "aggregations"}

DATEMATH = re.compile(r"^(now|\d{4}[-.\d]*[T\d:.]*Z?\|\|)")


def traits(requete):
    """L'ensemble des traits qu'une requete exerce.

    Un trait est ce qu'on sait compter **et** rattacher a une capacite :
    `route:search`, `dsl:match`, `dsl:match.fuzziness`, `agg:terms`,
    `corps:highlight`, `tri:missing`, `type:keyword`.
    """
    vus = set()
    api = requete.get("api")
    if api:
        vus.add(f"route:{api}")
    corps = requete.get("corps")
    if requete.get("chemin"):
        vus |= traits_expression(requete["chemin"])
    if isinstance(corps, dict):
        if api in FAMILLE_RECHERCHE or (api is None and ("query" in corps or "aggs" in corps)):
            vus |= traits_corps(corps)
        elif api in FAMILLE_QUERY_SEULE:
            source = corps.get("source") if isinstance(corps.get("source"), dict) else corps
            vus |= traits_requete(source.get("query") or {})
            vus |= traits_par_requete(api, corps)
        if api in ("indices.create", "indices.put_mapping", "indices.put_template",
                   "indices.put_index_template", "cluster.put_component_template"):
            vus |= traits_mapping(corps)
    if api in FAMILLE_QUERY_SEULE and requete.get("chemin"):
        vus |= traits_par_requete(api, None, requete["chemin"])
    for ligne in requete.get("corps_lignes") or []:
        if isinstance(ligne, dict) and ("query" in ligne or "aggs" in ligne):
            vus |= traits_corps(ligne)
    return vus


# Les parametres de `_delete_by_query` / `_update_by_query` / `_reindex` que
# compat.yaml declare un par un. Ils ne se lisent ni dans la requete DSL ni dans
# le corps d'une recherche : ce sont eux qui disent si une purge est jouable
# telle quelle, et sans eux le corpus comptait « servie » une purge en cinq
# tranches paralleles.
PARREQ_IGNORES = {"query", "source", "dest"}


def traits_par_requete(api, corps=None, chemin=None):
    """Ce qu'une commande par requete demande **autour** de sa requete.

    `slice`, `max_docs` et `conflicts` s'ecrivent dans le corps ; `slices`,
    `pipeline`, `routing`, `scroll_size` dans la query string. Les deux places
    comptent — le client officiel lui-meme n'est pas d'accord avec la
    documentation sur laquelle utiliser.
    """
    vus = set()
    if isinstance(corps, dict):
        for clef in corps:
            if clef not in PARREQ_IGNORES:
                vus.add(f"parreq:{api}.{clef}")
    if chemin and "?" in chemin:
        for paire in chemin.split("?", 1)[1].split("&"):
            nom = paire.split("=", 1)[0]
            if nom:
                vus.add(f"parreq:{api}.{nom}")
    return vus


INDEX_URL = re.compile(r"^/([^/_][^/]*)/")


def traits_expression(chemin):
    """Ce que l'expression d'index de l'URL demande : liste, joker, exclusion."""
    m = INDEX_URL.match(chemin)
    if not m:
        return {"expr:all"} if chemin.startswith("/_search") else set()
    expression = m.group(1)
    vus = set()
    if "," in expression:
        vus.add("expr:liste")
    if "*" in expression:
        vus.add("expr:joker")
    if any(part.startswith("-") for part in expression.split(",")):
        vus.add("expr:exclusion")
    if expression in ("_all", "*"):
        vus.add("expr:all")
    return vus


def traits_corps(corps):
    vus = set()
    for clef, valeur in corps.items():
        vus.add(f"corps:{clef}")
        if clef == "query" or clef == "post_filter":
            vus |= traits_requete(valeur)
        elif clef in CORPS_AGGS:
            vus |= traits_aggs(valeur)
        elif clef == "sort":
            vus |= traits_tri(valeur)
        elif clef == "highlight" and isinstance(valeur, dict):
            # Le bloc `highlight` se lit comme une clause : chacun de ses
            # reglages est un trait a part. Sans ca, un `type: fvh` — que
            # ferrite refuse — compterait comme un surlignage servi.
            vus |= traits_highlight(valeur)
        elif clef == "track_total_hits" and valeur is False:
            vus.add("corps:track_total_hits=false")
        elif clef in ("script_fields", "runtime_mappings") and valeur == {}:
            # Un objet **vide** ne definit aucun champ calcule : il ne demande
            # rien, et ES rend la meme reponse avec ou sans (mesure contre ES
            # 8.15). Ce n'est donc pas une demande, et ferrite l'accepte tel
            # quel — comme une clef absente.
            #
            # Ce cas n'est pas anecdotique et il est **compte a part** : 425
            # des 444 requetes du corpus qui portent `runtime_mappings`, et 349
            # des 359 qui portent `script_fields`, l'envoient vide. Ce sont des
            # gabarits de tracks Rally dont le parametre n'est pas rempli. Les
            # compter comme une demande refusee dirait que ferrite echoue la ou
            # il rend exactement la reponse d'ES ; les compter comme un support
            # de `script_fields` serait le mensonge inverse. Ils ne comptent ni
            # pour ni contre : la capacite reste ❌ dans la table.
            vus.discard(f"corps:{clef}")
    return vus


def traits_highlight(bloc):
    """Les reglages d'un bloc `highlight`, globaux et champ par champ."""
    vus = set()
    for clef, valeur in bloc.items():
        if clef == "fields":
            champs = valeur if isinstance(valeur, list) else [valeur]
            for entree in champs:
                if not isinstance(entree, dict):
                    continue
                for spec in entree.values():
                    if isinstance(spec, dict):
                        vus |= {f"corps:highlight.{k}" for k in spec}
            vus.add("corps:highlight.fields")
        else:
            vus.add(f"corps:highlight.{clef}")
    return vus


def traits_requete(noeud):
    """Descente dans l'arbre des clauses."""
    vus = set()
    if isinstance(noeud, list):
        for enfant in noeud:
            vus |= traits_requete(enfant)
        return vus
    if not isinstance(noeud, dict):
        return vus
    for nom, params in noeud.items():
        vus.add(f"dsl:{nom}")
        vus |= traits_parametres(nom, params)
        for sous in CONTENEURS.get(nom, ()):
            if isinstance(params, dict) and sous in params:
                vus |= traits_requete(params[sous])
        if nom not in CONTENEURS and nom not in CLAUSES_A_CHAMP and isinstance(params, dict):
            # une clause inconnue peut quand meme contenir une requete
            for clef in ("query", "filter"):
                if clef in params:
                    vus |= traits_requete(params[clef])
    return vus


# Les champs de **metadonnees** d'ES : les seuls noms qu'il reserve vraiment.
# Ce n'est pas le prefixe `_` — `_score`, `_doc`, `_type`, `_size`, `_all`,
# `_parent` et `_all_text` passent chez lui, mesure a l'appui, et ferrite les
# sert depuis qu'une vraie application (Wagtail) a montre qu'elle les employait.
# Compter le prefixe entier faisait passer pour refusee une requete que les deux
# serveurs servent.
METADONNEES = {"_id", "_index", "_source", "_routing", "_field_names", "_ignored",
               "_seq_no", "_version", "_nested_path", "_feature",
               "_data_stream_timestamp", "_tier"}


def traits_parametres(nom, params):
    vus = set()
    if not isinstance(params, dict):
        return vus
    if nom in CLAUSES_A_CHAMP:
        for champ, valeur in params.items():
            if champ in METADONNEES and champ not in ("_id", "_index"):
                vus.add("champ:reserve")
            if champ in ("boost", "_name", "case_insensitive"):
                vus.add(f"dsl:{nom}.{champ}")
            elif nom == "terms" and isinstance(valeur, dict):
                vus.add("dsl:terms.lookup" if set(valeur) & TERMS_LOOKUP else f"dsl:{nom}.?")
            elif isinstance(valeur, dict):
                for p, v in valeur.items():
                    vus.add(f"dsl:{nom}.{p}")
                    vus |= traits_valeur(nom, p, v)
    else:
        for p, v in params.items():
            if p in CONTENEURS.get(nom, ()):
                continue
            vus.add(f"dsl:{nom}.{p}")
            vus |= traits_valeur(nom, p, v)
        if nom == "multi_match" and any("*" in str(c) for c in params.get("fields") or []):
            vus.add("dsl:multi_match.fields=motif")
    return vus


# Les parametres dont c'est la **valeur** qui est declaree dans compat.yaml
# (`score_mode: max` est refuse, `score_mode: avg` ne l'est pas).
VALEUR_DECLAREE = {("nested", "score_mode"), ("multi_match", "type"),
                   ("has_child", "score_mode"), ("has_parent", "score_mode")}
BORNES = ("gt", "gte", "lt", "lte", "from", "to")


def traits_valeur(nom, param, valeur):
    vus = set()
    if (nom, param) in VALEUR_DECLAREE and isinstance(valeur, str):
        vus.add(f"dsl:{nom}.{param}={valeur}")
    if param in BORNES and isinstance(valeur, str) and DATEMATH.match(valeur):
        vus.add("datemath:now" if valeur.startswith("now") else "datemath:ancre")
        if re.search(r"[+-]\d+[yMwdhHms]", valeur):
            vus.add("datemath:decalage")
        if "/" in valeur:
            vus.add("datemath:arrondi")
    return vus


# Une agregation qui fait des seaux : `filter` sous l'une d'elles est refusee
# (il faudrait rejouer sa requete seau par seau), et compat.yaml le declare en
# toutes lettres plutot que par un nom de parametre.
AGGS_BUCKETS = {"terms", "range", "date_range", "histogram", "date_histogram",
                "auto_date_histogram", "filters", "significant_terms", "composite",
                "geohash_grid", "geotile_grid", "ip_range", "nested", "sampler",
                "diversified_sampler", "adjacency_matrix", "children", "parent",
                "variable_width_histogram", "multi_terms", "rare_terms"}


def traits_aggs(noeud, sous_bucket=False):
    vus = set()
    if not isinstance(noeud, dict):
        return vus
    for definition in noeud.values():
        if not isinstance(definition, dict):
            continue
        for nom, params in definition.items():
            if nom in MOTS_AGG:
                continue
            vus.add(f"agg:{nom}")
            if nom == "filter" and sous_bucket:
                vus.add("agg:filter.sous_bucket")
            if isinstance(params, dict) and \
                    params.get("field") in METADONNEES - {"_index", "_id"}:
                vus.add("champ:reserve")
            if isinstance(params, dict):
                for p in params:
                    vus.add(f"agg:{nom}.{p}")
                if nom == "terms":
                    if isinstance(params.get("order"), (dict, list)):
                        ordres = (params["order"]
                                  if isinstance(params["order"], list)
                                  else [params["order"]])
                        for ordre in ordres:
                            if not isinstance(ordre, dict):
                                continue
                            # L'ordre par une sous-agregation metrique est
                            # servi ; le chemin **a plusieurs niveaux**, qui
                            # traverse une agregation mono-seau, ne l'est pas.
                            if any(">" in c for c in ordre):
                                vus.add("agg:terms.order=chemin_multi")
                    for p in ("include", "exclude"):
                        v = params.get(p)
                        if v is None:
                            continue
                        # La forme partitionnee, et la cohabitation avec
                        # `missing` : deux refus declares qu'aucun nom de
                        # parametre ne porte.
                        if isinstance(v, dict):
                            vus.add("agg:terms.filtre=partition")
                        if "missing" in params:
                            vus.add("agg:terms.filtre_et_missing")
                if nom == "filter":
                    vus |= traits_requete(params)
        for clef in CORPS_AGGS:
            if clef in definition:
                vus.add("agg:sous_agregations")
                vus |= traits_aggs(definition[clef],
                                   sous_bucket=any(n in AGGS_BUCKETS for n in definition))
    return vus


def traits_tri(noeud):
    vus = {"corps:sort"}
    elements = noeud if isinstance(noeud, list) else [noeud]
    for element in elements:
        if isinstance(element, str):
            continue
        if not isinstance(element, dict):
            continue
        for champ, options in element.items():
            if champ == "_script":
                vus.add("tri:script")
            elif champ == "_geo_distance":
                vus.add("tri:geo")
            if isinstance(options, dict):
                for p in options:
                    if p != "order":
                        vus.add(f"tri:{p}")
    return vus


TYPES_CHAMP = {"text", "keyword", "long", "integer", "short", "byte", "double", "float",
               "half_float", "scaled_float", "boolean", "date", "object", "nested", "join"}


def traits_mapping(corps, prefixe=""):
    """Ce qu'un corps de creation d'index declare : types de champ, analyzers."""
    vus = set()
    mappings = corps.get("mappings") or corps.get("template", {}).get("mappings") or corps
    if not isinstance(mappings, dict):
        return vus
    proprietes = mappings.get("properties")
    if isinstance(proprietes, dict):
        for definition in proprietes.values():
            if not isinstance(definition, dict):
                continue
            type_champ = definition.get("type")
            if type_champ:
                vus.add(f"type:{type_champ}")
            elif "properties" in definition:
                vus.add("type:object")
            for param, valeur in definition.items():
                if param in ("type", "properties"):
                    continue
                vus.add(f"champ:{param}")
                if param in ("analyzer", "search_analyzer") and isinstance(valeur, str):
                    vus.add(f"analyzer:{valeur}")
            if "properties" in definition:
                vus |= traits_mapping({"properties": definition["properties"]})
            if isinstance(definition.get("fields"), dict):
                vus |= traits_mapping({"properties": definition["fields"]})
    analyse = (corps.get("settings") or {}).get("analysis") if isinstance(corps.get("settings"), dict) else None
    if isinstance(analyse, dict):
        for famille in ("analyzer", "tokenizer", "filter", "char_filter", "normalizer"):
            if famille in analyse:
                vus.add(f"analyse:{famille}")
    return vus


# ===========================================================================
# 2. Du trait a la capacite declaree
# ===========================================================================

# Les clauses que compat.yaml nomme une par une. Le reste du Query DSL tombe
# dans `dsl.non_supportees`, qui les refuse toutes avec le meme message
# (`unknown query [...]`) : c'est deja ce que fait le rapport de conformance.
CLAUSES = {
    "match_all": "dsl.match_all", "match_none": "dsl.match_none", "match": "dsl.match",
    "multi_match": "dsl.multi_match", "match_phrase": "dsl.match_phrase",
    "match_phrase_prefix": "dsl.match_phrase_prefix", "exists": "dsl.exists",
    "term": "dsl.term", "ids": "dsl.ids", "prefix": "dsl.prefix", "wildcard": "dsl.wildcard",
    "regexp": "dsl.regexp", "fuzzy": "dsl.fuzzy", "constant_score": "dsl.constant_score",
    "dis_max": "dsl.dis_max", "terms": "dsl.terms", "range": "dsl.range", "bool": "dsl.bool",
    "function_score": "dsl.function_score", "boosting": "dsl.boosting",
    "query_string": "dsl.query_string", "simple_query_string": "dsl.simple_query_string",
    "nested": "nested.clause", "has_child": "join.has_child", "has_parent": "join.has_child",
    "parent_id": "join.parent_id",
}
CLAUSES_DEFAUT = "dsl.non_supportees"

# Les clefs de corps de `_search` que compat.yaml nomme. Le reste tombe dans
# `recherche.non_supportes` **s'il y figure**, sinon il est indetermine — et un
# indetermine compte contre nous.
CORPS = {
    "query": "recherche.query", "size": "recherche.from_size", "from": "recherche.from_size",
    "sort": "recherche.sort", "_source": "recherche.source",
    "track_total_hits": "recherche.track_total_hits", "aggs": "recherche.aggs",
    "aggregations": "recherche.aggs",
    "fields": "recherche.fields", "docvalue_fields": "recherche.docvalue_fields",
    "highlight": "recherche.highlight",
    "stored_fields": "recherche.stored_fields",
    "script_fields": "recherche.script_fields",
    "runtime_mappings": "recherche.script_fields",
    # Sa capacite est `partiel` — accepte, verifie, sans objet — donc elle est
    # nommee ici et non laissee dans `recherche.non_supportes`. La quitter sans
    # la rattacher l'aurait fait tomber dans « aucune capacite ne la reclame »,
    # qui compte contre nous : 94 requetes du corpus la posent.
    "timeout": "recherche.timeout",
    # Servi depuis la carte 41 : il a sa capacite, donc il quitte la liste des
    # non supportes. Sans ce rattachement, les requetes du corpus qui le posent
    # tomberaient dans « aucune capacite ne la reclame », qui compte **contre**
    # nous — c'est le garde-fou du troisieme verdict.
    "min_score": "recherche.min_score",
    # Troisieme fois, meme piege : servis depuis la carte 08, donc ils ont
    # chacun leur capacite et quittent la liste des non supportes. Sans ce
    # rattachement, les requetes du corpus qui les posent tomberaient dans
    # « aucune capacite ne la reclame » — et le taux **baisserait** en livrant la
    # fonctionnalite. Mesure : 6 requetes posent `search_after`, 6 posent `pit`.
    "search_after": "recherche.search_after",
    "pit": "recherche.pit",
}
# La liste des clefs refusees n'est pas recopiee ici : elle se lit dans le
# `nom` de la capacite `recherche.non_supportes`, qui les cite entre accents
# graves. Une clef que compat.yaml ne nomme pas reste indeterminee, et compte
# donc contre nous.
CORPS_NON_SUPPORTES = "recherche.non_supportes"

AGGS = {
    "min": "agg.metriques", "max": "agg.metriques", "sum": "agg.metriques",
    "avg": "agg.metriques", "value_count": "agg.metriques", "stats": "agg.metriques",
    "terms": "agg.terms", "range": "agg.range", "histogram": "agg.histogram",
    "date_histogram": "agg.date_histogram", "filter": "agg.filter",
    "cardinality": "agg.cardinality",
    "extended_stats": "agg.extended_stats", "percentiles": "agg.percentiles",
    "top_hits": "agg.top_hits",
    # pas un nom d'agregation : le trait que leve une agregation qui en
    # contient une autre. compat.yaml le declare comme une capacite a part.
    "sous_agregations": "agg.sous_agregations",
}
AGGS_DEFAUT = "agg.non_supportees"

TYPES = {"text": "type.text", "keyword": "type.keyword", "long": "type.entiers",
         "integer": "type.entiers", "short": "type.entiers", "byte": "type.entiers",
         "double": "type.flottants", "float": "type.flottants", "boolean": "type.boolean",
         "date": "type.date", "object": "type.object", "nested": "type.nested",
         "join": "join.type"}
TYPES_DEFAUT = "type.autres"

ANALYZERS = {"standard": "analyzer.standard", "simple": "analyzer.simple",
             "whitespace": "analyzer.whitespace", "keyword": "analyzer.keyword",
             "stop": "analyzer.stop", "english": "analyzer.english",
             "french": "analyzer.french", "snowball": "analyzer.snowball",
             "finnish": "analyzer.finnish"}
# Les douze analyzers de langue servis comptent pour **une** capacite, celle
# sous laquelle ils sont declares.
ANALYZERS.update({l: "analyzer.langues" for l in (
    "danish", "dutch", "german", "hungarian", "italian", "norwegian",
    "portuguese", "romanian", "russian", "spanish", "swedish", "turkish")})
# Les analyzers de langue d'ES, nommes un par un. Tout autre nom est celui d'un
# analyzer **declare par l'index** : `settings.analysis` l'a pose dans une
# requete precedente, que ce corps-ci ne porte pas. On ne peut donc pas trancher
# depuis la requete seule, et le verdict est `indeterminee` — qui compte contre
# nous, comme dans le rapport de conformance.
#
# Longtemps, tout nom inconnu tombait dans « analyzer de langue », donc refuse :
# la campagne Wagtail affichait « trait refuse : analyzer:edgengram_analyzer »
# pour un analyzer que l'index venait de declarer et que ferrite sert.
ANALYZERS_LANGUE = {
    "arabic", "armenian", "basque", "bengali", "brazilian", "bulgarian", "catalan", "cjk",
    "czech", "estonian", "galician", "greek", "hindi", "indonesian", "irish", "latvian",
    "lithuanian", "persian", "serbian", "sorani", "thai", "pattern", "fingerprint",
}

CHAMPS = {"fields": "type.multi_fields", "ignore_above": "type.ignore_above",
          "analyzer": "type.analyzer", "search_analyzer": "type.search_analyzer",
          "copy_to": "type.copy_to", "store": "type.store",
          "index": "type.index"}
CHAMPS_DEFAUT = "type.autres_parametres"

EXPRESSIONS = {"expr:liste": "expr.liste", "expr:joker": "expr.joker",
               "expr:all": "expr.all", "expr:exclusion": "expr.exclusion"}
DATE_MATH = {"datemath:now": "datemath.now", "datemath:decalage": "datemath.decalage",
             "datemath:arrondi": "datemath.arrondi", "datemath:ancre": "datemath.ancre"}
ANALYSE = {"analyse:analyzer": "analyse.custom", "analyse:tokenizer": "analyse.tokenizers",
           "analyse:filter": "analyse.filtres", "analyse:char_filter": "analyse.char_filter",
           "analyse:normalizer": "analyse.char_filter"}

# Ce qu'aucun nom de parametre ne dit : ces traits-la sont refuses, et par qui.
# Chacun correspond a une ligne `texte:` de compat.yaml, c'est-a-dire a un refus
# declare que seule une regle peut reconnaitre.
TRAITS_REFUSES = {
    "dsl:terms.lookup": "dsl.terms",            # « les *terms lookup* »
    # L'ordre par une sous-agregation metrique est servi depuis qu'il est
    # mesure ; ce qui reste refuse est le chemin a plusieurs niveaux, la forme
    # partitionnee d'un filtre, et un filtre pose en meme temps qu'un `missing`.
    "agg:terms.order=chemin_multi": "agg.terms",
    "agg:terms.filtre=partition": "agg.terms",
    "agg:terms.filtre_et_missing": "agg.terms",
    "tri:script": "recherche.sort",             # « le tri par script »
    "tri:geo": "recherche.sort",
    "dsl:multi_match.type=cross_fields": "libre.cross_fields",
    "dsl:multi_match.type=bool_prefix": "libre.cross_fields",
    "dsl:multi_match.fields=motif": "dsl.multi_match",   # « les motifs de champ (`tit*`) »
    "agg:filter.sous_bucket": "agg.filter",              # « sous une agregation de buckets »
    "champ:reserve": "type.noms_reserves",               # `_type`, `_tsid`, `_ignored`…
    # la jointure rend un score constant : tout `score_mode` autre que `none`
    # est refuse, et c'est une note de compat.yaml qu'aucun nom ne porte
    "dsl:has_child.score_mode=min": "join.has_child",
    "dsl:has_child.score_mode=max": "join.has_child",
    "dsl:has_child.score_mode=sum": "join.has_child",
    "dsl:has_child.score_mode=avg": "join.has_child",
    "dsl:has_parent.score_mode=min": "join.has_child",
    "dsl:has_parent.score_mode=max": "join.has_child",
    "dsl:has_parent.score_mode=sum": "join.has_child",
    "dsl:has_parent.score_mode=avg": "join.has_child",
}
# Un parametre de clause que compat.yaml declare sur une capacite **a part** :
# le refus de `inner_hits` ne se lit pas sur `nested.clause`, il a sa propre
# ligne. Sans ca, le croisement dirait « servie » la ou ferrite refuse.
SOUS_CAPACITES = {
    "dsl:nested.inner_hits": "nested.inner_hits",
    "dsl:nested.ignore_unmapped": "nested.inner_hits",
    "dsl:nested.score_mode": "nested.score_mode",
    "dsl:has_child.score_mode": "join.has_child",
    "dsl:has_parent.score_mode": "join.has_child",
    "dsl:multi_match.slop": "libre.slop",
}
TRAITS_SERVIS = {
    "dsl:has_child.score_mode=none": "join.has_child",
    "dsl:has_parent.score_mode=none": "join.has_child",
    "dsl:multi_match.type=best_fields": "libre.best_fields",
    "dsl:multi_match.type=most_fields": "libre.most_fields",
    "dsl:multi_match.type=phrase": "libre.phrase",
    "dsl:multi_match.type=phrase_prefix": "libre.phrase_prefix",
    "dsl:match.lenient": "libre.lenient",
    "dsl:multi_match.lenient": "libre.lenient",
}


class Croisement:
    """Le pont entre un trait du corpus et une capacite de compat.yaml."""

    def __init__(self):
        self._cites = {}
        self.doc = genere_compat.charge()
        self.perimetre = mod_perimetre.Perimetre(self.doc)
        self.capacites = self.perimetre.capacites

    # -- rattachement --------------------------------------------------------

    def capacite(self, trait):
        """(id de capacite, valeur declaree du parametre) ou (None, None)."""
        if trait in TRAITS_REFUSES:
            return TRAITS_REFUSES[trait], None
        if trait in TRAITS_SERVIS:
            return TRAITS_SERVIS[trait], None
        famille, _, reste = trait.partition(":")
        if famille == "route":
            cid, _ = self.perimetre.capacite_de(reste, "")
            return cid, None
        if famille == "dsl":
            clause, _, param = reste.partition(".")
            sans_valeur = trait.split("=", 1)[0]
            if sans_valeur in SOUS_CAPACITES:
                return SOUS_CAPACITES[sans_valeur], (trait.split("=", 1)[1]
                                                     if "=" in trait else param)
            if param.split("=")[0] == "_name":
                return "dsl.nom_de_clause", None
            cid = CLAUSES.get(clause, CLAUSES_DEFAUT if clause_connue(clause) else None)
            valeur = param.split("=", 1)[1] if "=" in param else param
            return cid, valeur or None
        if famille == "parreq":
            api, _, param = reste.partition(".")
            cid, _ = self.perimetre.capacite_de(api, "")
            return cid, (param or None)
        if famille == "agg":
            nom, _, param = reste.partition(".")
            return AGGS.get(nom, AGGS_DEFAUT), (param or None)
        if famille == "corps":
            clef, _, valeur = reste.partition("=")
            # `corps:highlight.type` : un reglage du bloc de surlignage. Le
            # verdict se lit sur les parametres declares de la capacite, comme
            # pour un parametre de clause.
            if clef.startswith("highlight."):
                return "recherche.highlight", clef.split(".", 1)[1]
            if clef in CORPS:
                return CORPS[clef], (valeur or None)
            if clef in self.noms_cites(CORPS_NON_SUPPORTES):
                return CORPS_NON_SUPPORTES, None
            return None, None
        if famille == "tri":
            return "recherche.sort", reste
        if famille == "type":
            return TYPES.get(reste, TYPES_DEFAUT), None
        if famille == "champ":
            return CHAMPS.get(reste, CHAMPS_DEFAUT), None
        if famille == "analyzer":
            if reste in ANALYZERS:
                return ANALYZERS[reste], None
            return ("analyzer.autres_langues" if reste in ANALYZERS_LANGUE else None), None
        if famille == "analyse":
            return ANALYSE.get(trait), None
        if famille == "expr":
            return EXPRESSIONS.get(trait), None
        if famille == "datemath":
            return DATE_MATH.get(trait), None
        return None, None

    # -- verdict -------------------------------------------------------------

    def verdict(self, trait):
        """(verdict, capacite). Un trait qu'aucune capacite ne reclame est
        `indeterminee` : il compte contre nous, comme dans le rapport de
        conformance."""
        cid, param = self.capacite(trait)
        if cid is None:
            return INDETERMINE, None
        if trait in TRAITS_REFUSES:
            return REFUSE, cid
        cap = self.capacites[cid]
        if cap["etat"] == "refuse":
            return REFUSE, cid
        if cap["etat"] == "partiel" and param:
            if param in self.noms_declares(cap, "refuses"):
                return REFUSE, cid
            supportes = self.noms_declares(cap, "supportes")
            # Un parametre que ni la colonne « supporté » ni la colonne
            # « refusé » ne nomme n'est pas servi : il n'est **pas declare**.
            # Compter le silence pour un oui rendrait le taux flatteur des
            # qu'on oublie une ligne — c'est la meme regle que l'`indetermine`
            # du rapport de conformance.
            if supportes and param not in supportes:
                return INDETERMINE, cid
        return SERVI, cid

    def noms_cites(self, cid):
        """Les identifiants qu'une capacite cite entre accents graves dans son
        nom : c'est la que `recherche.non_supportes` dit *quelles* clefs."""
        if cid not in self._cites:
            self._cites[cid] = set(ACCENTS_GRAVES.findall(self.capacites[cid]["nom"]))
        return self._cites[cid]

    @staticmethod
    def noms_declares(cap, cle):
        """Les noms de parametres d'une liste `supportes` / `refuses`.

        Une entree porte soit un `nom`, soit un `texte` qui cite un ou
        plusieurs parametres entre accents graves (« `offset`, `min_doc_count`…
        comme `histogram` ») : les deux comptent, sinon la moitie des
        parametres declares passerait pour non declaree.
        """
        noms = set()
        for p in (cap.get("parametres") or {}).get(cle) or []:
            if isinstance(p, str):
                noms.add(p)
            elif p.get("nom"):
                noms.add(p["nom"])
            elif p.get("texte"):
                noms |= set(ACCENTS_GRAVES.findall(p["texte"]))
        return noms


ACCENTS_GRAVES = re.compile(r"`([A-Za-z_][A-Za-z0-9_.]*)`")
CLAUSES_INCONNUES_TOLEREES = {"boost", "_name"}


def clause_connue(nom):
    """Une clause du Query DSL, ou une clef qu'on a prise pour une clause ?

    `dsl.non_supportees` declare refuser toute clause inconnue avec
    `unknown query [...]` : lui rattacher n'importe quelle clef serait donc
    exact pour ferrite, mais ferait disparaitre nos propres erreurs de lecture.
    Les deux seules clefs qui ne sont pas des clauses sont ecartees a la main.
    """
    return nom not in CLAUSES_INCONNUES_TOLEREES


# ===========================================================================
# 3. La mesure
# ===========================================================================

def charge_corpus(chemin):
    with open(chemin, encoding="utf-8") as f:
        return [json.loads(ligne) for ligne in f if ligne.strip()]


def mesure(corpus, croisement):
    """Le verdict declare, requete par requete."""
    resultats = []
    for requete in corpus:
        ses_traits = sorted(traits(requete))
        manques, inconnus, capacites = [], [], set()
        for trait in ses_traits:
            verdict, cid = croisement.verdict(trait)
            if cid:
                capacites.add(cid)
            if verdict == REFUSE:
                manques.append({"trait": trait, "capacite": cid})
            elif verdict == INDETERMINE:
                inconnus.append(trait)
        if manques:
            verdict = REFUSE
        elif inconnus:
            verdict = INDETERMINE
        else:
            verdict = SERVI
        resultats.append({"ref": requete.get("ref"), "source": requete.get("source"),
                          "api": requete.get("api"), "verdict": verdict,
                          "traits": ses_traits, "capacites": sorted(capacites),
                          "manques": manques, "traits_non_rattaches": inconnus})
    return resultats


def compte(resultats):
    """Distribution, poids, et la table « ce qui manque, par frequence »."""
    total = len(resultats)
    par_trait = collections.Counter()
    par_capacite = collections.Counter()
    manques_par_capacite = collections.Counter()
    manques_par_trait = collections.Counter()
    non_rattaches = collections.Counter()
    verdicts = collections.Counter()
    par_source = collections.defaultdict(collections.Counter)
    for res in resultats:
        verdicts[res["verdict"]] += 1
        par_source[res["source"]][res["verdict"]] += 1
        par_source[res["source"]]["total"] += 1
        for trait in res["traits"]:
            par_trait[trait] += 1
        for cid in res["capacites"]:
            par_capacite[cid] += 1
        # une capacite ne compte qu'une fois par requete : `top_hits` et son
        # `size` sont un seul manque, pas deux, sinon la part depasse le corpus
        for cid in {m["capacite"] for m in res["manques"]}:
            manques_par_capacite[cid] += 1
        for trait in {m["trait"] for m in res["manques"]}:
            manques_par_trait[trait] += 1
        for trait in res["traits_non_rattaches"]:
            non_rattaches[trait] += 1
    return {"total": total, "verdicts": verdicts, "par_trait": par_trait,
            "par_capacite": par_capacite, "manques_par_capacite": manques_par_capacite,
            "manques_par_trait": manques_par_trait, "non_rattaches": non_rattaches,
            "par_source": par_source}


def poids_mesures(comptes, croisement):
    """poids = part des requetes du corpus qui exercent la capacite (%).

    Une capacite qu'aucun trait ne sait exercer garde `null` : le corpus ne
    peut rien en dire, et un 0 laisserait croire qu'elle a ete mesuree."""
    total = comptes["total"] or 1
    mesurables = capacites_mesurables(croisement)
    poids = {}
    for cid in croisement.capacites:
        if cid not in mesurables:
            poids[cid] = None
        else:
            poids[cid] = round(100.0 * comptes["par_capacite"].get(cid, 0) / total, 1)
    return poids


def capacites_mesurables(croisement):
    """Les capacites qu'un trait peut atteindre — c'est-a-dire ce que ce corpus
    sait voir. Le reste (forme de reponse, scoring, mapping dynamique…) n'est
    pas visible dans une requete."""
    atteignables = set()
    for table in (CLAUSES, CORPS, AGGS, TYPES, ANALYZERS, CHAMPS, EXPRESSIONS,
                  DATE_MATH, ANALYSE, TRAITS_REFUSES, TRAITS_SERVIS):
        atteignables |= set(table.values())
    atteignables |= {CLAUSES_DEFAUT, AGGS_DEFAUT, TYPES_DEFAUT, CHAMPS_DEFAUT,
                     "analyzer.autres_langues", "recherche.non_supportes",
                     "recherche.sort"}
    atteignables |= set(croisement.perimetre.apis.values())
    atteignables |= {cid for _, cid in croisement.perimetre.familles}
    return atteignables & set(croisement.capacites)


# ===========================================================================
# 4. Le rejeu contre les deux serveurs
# ===========================================================================

INDEX_REJEU = "ponderation"


def appel(url, methode="GET", corps=None, delai=30):
    donnees = None if corps is None else json.dumps(corps).encode()
    req = urllib.request.Request(url, data=donnees, method=methode)
    req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=delai) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        brut = e.read()
        try:
            return e.code, json.loads(brut or b"{}")
        except ValueError:
            return e.code, {"brut": brut[:200].decode("utf-8", "replace")}
    except Exception as e:  # noqa: BLE001
        return 0, {"erreur": str(e)}


def raison_erreur(corps):
    """Le message d'erreur, au format d'ES comme a celui de ferrite."""
    erreur = (corps or {}).get("error")
    if isinstance(erreur, dict):
        return (erreur.get("reason") or "")[:200]
    return str(erreur)[:200] if erreur else ""


# -- le mapping que la requete suppose ---------------------------------------
#
# Rejouer contre un index **sans mapping** ne mesure pas ce qu'on croit : ES
# rend des buckets vides pour une agregation sur un champ inconnu, ferrite la
# refuse — la mesure aurait donc compte 142 refus qui ne parlent que de
# l'absence de mapping. Chaque requete est donc rejouee contre un index qui
# porte **les champs qu'elle nomme**, du type que sa propre forme suppose.
#
# Le meme mapping est pose sur les deux serveurs : une inference de travers ne
# peut que sortir la requete du denominateur (ES la refusera aussi), jamais
# flatter ferrite.
RANG_TYPE = {"text": 0, "keyword": 1, "boolean": 2, "double": 3, "date": 4}
CLAUSES_TEXTE = {"match", "match_phrase", "match_phrase_prefix", "match_bool_prefix",
                 "common", "intervals"}
CLAUSES_CLEF = {"term", "terms", "prefix", "wildcard", "regexp", "fuzzy", "exists",
                "span_term", "terms_set"}
AGGS_NOMBRE = {"avg", "sum", "min", "max", "stats", "extended_stats", "histogram",
               "percentiles", "percentile_ranks", "median_absolute_deviation",
               "weighted_avg", "boxplot"}
AGGS_DATE = {"date_histogram", "auto_date_histogram", "date_range"}
ISO_DATE = re.compile(r"^\d{4}-\d{2}-\d{2}([T ]|$)|^now")


def type_devine(valeur, defaut="keyword"):
    if isinstance(valeur, bool):
        return "boolean"
    if isinstance(valeur, (int, float)):
        return "double"
    if isinstance(valeur, str) and ISO_DATE.match(valeur):
        return "date"
    return defaut


def champs_supposes(corps):
    """{chemin de champ: type} — ce que la requete suppose du mapping."""
    champs = {}

    def pose(champ, type_champ):
        if not isinstance(champ, str) or not champ or champ.startswith("_") \
                or "*" in champ or champ.endswith("."):
            return
        ancien = champs.get(champ)
        if ancien is None or RANG_TYPE.get(type_champ, 1) > RANG_TYPE.get(ancien, 1):
            champs[champ] = type_champ

    def requete(noeud):
        if isinstance(noeud, list):
            for enfant in noeud:
                requete(enfant)
            return
        if not isinstance(noeud, dict):
            return
        for nom, params in noeud.items():
            if nom in CLAUSES_TEXTE or nom in CLAUSES_CLEF:
                defaut = "text" if nom in CLAUSES_TEXTE else "keyword"
                if nom == "exists" and isinstance(params, dict):
                    pose(params.get("field"), "keyword")
                elif isinstance(params, dict):
                    for champ, valeur in params.items():
                        if isinstance(valeur, dict):
                            valeur = valeur.get("query", valeur.get("value", ""))
                        if isinstance(valeur, list):
                            valeur = valeur[0] if valeur else ""
                        pose(champ, type_devine(valeur, defaut) if nom != "match" else defaut)
            elif nom == "range" and isinstance(params, dict):
                for champ, bornes in params.items():
                    borne = ""
                    if isinstance(bornes, dict):
                        for b in ("gte", "gt", "lte", "lt", "from", "to"):
                            if bornes.get(b) is not None:
                                borne = bornes[b]
                                break
                    pose(champ, type_devine(borne, "double"))
            elif nom == "multi_match" and isinstance(params, dict):
                for champ in params.get("fields") or []:
                    pose(str(champ).split("^")[0], "text")
            elif nom == "nested" and isinstance(params, dict):
                pose(params.get("path"), "nested")
                requete(params.get("query"))
            else:
                for sous in CONTENEURS.get(nom, ()):
                    if isinstance(params, dict):
                        requete(params.get(sous))
                if isinstance(params, dict) and nom not in CONTENEURS:
                    for clef in ("query", "filter"):
                        requete(params.get(clef))

    def aggregations(noeud):
        if not isinstance(noeud, dict):
            return
        for definition in noeud.values():
            if not isinstance(definition, dict):
                continue
            for nom, params in definition.items():
                if nom in CORPS_AGGS:
                    aggregations(params)
                elif isinstance(params, dict) and params.get("field"):
                    if nom in AGGS_DATE:
                        pose(params["field"], "date")
                    elif nom in AGGS_NOMBRE:
                        pose(params["field"], "double")
                    else:
                        pose(params["field"], "keyword")
                elif nom == "filter":
                    requete(params)

    requete(corps.get("query"))
    requete(corps.get("post_filter"))
    for clef in CORPS_AGGS:
        aggregations(corps.get(clef))
    tri = corps.get("sort")
    for element in (tri if isinstance(tri, list) else [tri]):
        if isinstance(element, str) and not element.startswith("_"):
            pose(element, "keyword")
        elif isinstance(element, dict):
            for champ in element:
                pose(champ, "keyword")
    return champs


def mapping_de(champs):
    """Les chemins pointes deviennent des objets : ferrite refuse un nom de
    champ qui contient un point, exactement comme il refuse `a.b` a
    l'indexation."""
    proprietes = {}
    for champ, type_champ in sorted(champs.items()):
        courant = proprietes
        morceaux = champ.split(".")
        for morceau in morceaux[:-1]:
            noeud = courant.setdefault(morceau, {"properties": {}})
            if "properties" not in noeud:
                noeud.clear()
                noeud["properties"] = {}
            courant = noeud["properties"]
        feuille = morceaux[-1]
        if feuille in courant and "properties" in courant[feuille]:
            continue
        courant[feuille] = ({"type": "nested", "properties": {}} if type_champ == "nested"
                            else {"type": type_champ})
    return {"mappings": {"properties": proprietes}}


def prepare_index(url, nom, mapping):
    appel(f"{url}/{nom}", "DELETE")
    return appel(f"{url}/{nom}", "PUT", mapping)


def rejoue(corpus, resultats, url_ferrite, url_es, journal=None):
    """La meme requete aux deux serveurs, sur le meme index vide.

    Ce qui est compare, c'est **accepte / refuse** : les deux index sont vides,
    donc il n'y a pas de documents a departager — c'est bien la question posee,
    « est-ce que ce corps-la passerait ». Une requete qu'ES refuse **aussi**
    sort du denominateur : elle ne dit rien de ferrite, ni dans un sens ni dans
    l'autre (une syntaxe d'une autre version, un champ que l'inference a mal
    type, une clause qui exige un etat qu'on n'a pas monte).
    """
    compte_rejeu = collections.Counter()
    # Les requetes sont regroupees par mapping suppose : un seul index vit a la
    # fois sur chaque serveur, la ou un index par requete en laisserait des
    # milliers (et depasserait la limite de shards d'un ES mono-noeud).
    groupes = collections.OrderedDict()
    for requete, res in zip(corpus, resultats):
        corps = requete.get("corps")
        if requete.get("api") != "search" or not isinstance(corps, dict) or not corps:
            continue
        mapping = mapping_de(champs_supposes(corps))
        groupes.setdefault(json.dumps(mapping, sort_keys=True), []).append((corps, res))
    for signature, requetes in groupes.items():
        mapping = json.loads(signature)
        sf, cf = prepare_index(url_ferrite, INDEX_REJEU, mapping)
        se, ce = prepare_index(url_es, INDEX_REJEU, mapping)
        if sf >= 400 or se >= 400:
            # Un mapping qu'un des deux refuse : ces requetes-la sont mesurees
            # contre un index **sans** mapping des deux cotes, et le refus est
            # compte a part plutot que de disparaitre dans le total.
            compte_rejeu["mappings_refuses"] += 1
            if journal is not None:
                journal.append({"ferrite": sf, "es": se, "requetes": len(requetes),
                                "erreur_ferrite": raison_erreur(cf),
                                "erreur_es": raison_erreur(ce), "mapping": mapping})
            prepare_index(url_ferrite, INDEX_REJEU, {})
            prepare_index(url_es, INDEX_REJEU, {})
        for corps, res in requetes:
            chemin = f"/{INDEX_REJEU}/_search"
            sf, cf = appel(url_ferrite + chemin, "POST", corps)
            se, ce = appel(url_es + chemin, "POST", corps)
            res["rejeu"] = {"ferrite": sf, "es": se}
            if sf >= 400:
                res["rejeu"]["ferrite_erreur"] = raison_erreur(cf)
            if se >= 400:
                res["rejeu"]["es_erreur"] = raison_erreur(ce)
            if se >= 400:
                etat = "hors_mesure" if sf >= 400 else "acceptee_par_ferrite_seul"
            else:
                etat = "servie" if sf < 400 else "refusee"
            res["rejeu"]["etat"] = etat
            compte_rejeu[etat] += 1
            compte_rejeu["total"] += 1
    compte_rejeu["mappings"] = len(groupes)
    appel(f"{url_ferrite}/{INDEX_REJEU}", "DELETE")
    appel(f"{url_es}/{INDEX_REJEU}", "DELETE")
    return compte_rejeu


# ===========================================================================
# 5. Rapport, poids, verification
# ===========================================================================

def par_frequence(compte, combien=None):
    """Les entrees d'un compteur, de la plus frequente a la moins — et, a
    egalite, par ordre alphabetique.

    `Counter.most_common()` laisse les ex aequo dans leur ordre d'insertion,
    qui depend de l'ordre de parcours des ensembles : deux lancers du meme
    fichier sur le meme corpus rendaient deux `usage.json` differents. Un
    rapport publie qui bouge tout seul rend illisible celui qui bouge pour une
    raison.
    """
    items = sorted(compte.items(), key=lambda kv: (-kv[1], kv[0]))
    return items[:combien] if combien else items


def rapport(corpus, resultats, comptes, croisement, poids, compte_rejeu, sources):
    total = comptes["total"]
    servies = comptes["verdicts"][SERVI]
    manques = []
    for cid, n in par_frequence(comptes["manques_par_capacite"]):
        cap = croisement.capacites[cid]
        detail, par_source = collections.Counter(), collections.Counter()
        for res in resultats:
            traits_ici = {m["trait"] for m in res["manques"] if m["capacite"] == cid}
            for trait in traits_ici:
                detail[trait] += 1
            if traits_ici:
                par_source[res["source"]] += 1
        manques.append({
            "capacite": cid, "nom": genere_compat.sans_balises(cap["nom"]),
            "etat": cap["etat"], "motif": cap.get("motif"),
            "requetes": n, "part": round(100.0 * n / (total or 1), 1),
            # La meme table par source : le corpus n'est pas homogene, et une
            # priorite tiree de la seule documentation classerait en tete ce
            # dont la doc parle, pas ce que les applications envoient.
            "par_source": {s: {"requetes": k,
                               "part": round(100.0 * k / (comptes["par_source"][s]["total"] or 1), 1)}
                           for s, k in sorted(par_source.items())},
            "traits": [{"trait": t, "requetes": c} for t, c in par_frequence(detail)],
        })
    rejeu = None
    if compte_rejeu:
        # Le denominateur, ce sont les requetes qu'un vrai ES 8.15 accepte dans
        # les conditions de la mesure (index vide, aucun mapping). Ce qu'il
        # refuse ne dit rien de ferrite : ni dans un sens, ni dans l'autre.
        denominateur = compte_rejeu["servie"] + compte_rejeu["refusee"]
        rejeu = {
            "requetes_rejouees": compte_rejeu["total"],
            "es_refuse_aussi": compte_rejeu["hors_mesure"],
            "acceptees_par_ferrite_seul": compte_rejeu["acceptee_par_ferrite_seul"],
            "denominateur": denominateur,
            "servies": compte_rejeu["servie"],
            "refusees": compte_rejeu["refusee"],
            "taux": round(100.0 * compte_rejeu["servie"] / (denominateur or 1), 1),
            "mappings_supposes": compte_rejeu["mappings"],
            "mappings_refuses": compte_rejeu["mappings_refuses"],
            "erreurs_de_ferrite": erreurs_frequentes(resultats),
            "accord_avec_le_croisement": accord(resultats),
        }
    return {
        "schema": 1,
        "corpus": sources,
        "totaux": {
            "requetes": total,
            "servies": servies,
            "refusees": comptes["verdicts"][REFUSE],
            "indeterminees": comptes["verdicts"][INDETERMINE],
            "taux_servies": round(100.0 * servies / (total or 1), 1),
        },
        "par_source": {s: dict(c, taux_servies=round(100.0 * c[SERVI] / (c["total"] or 1), 1))
                       for s, c in sorted(comptes["par_source"].items())},
        "concentration": concentration(corpus),
        "par_api": distribution(resultats, "api"),
        "distribution": {
            "traits": [{"trait": t, "requetes": n, "part": round(100.0 * n / (total or 1), 1)}
                       for t, n in par_frequence(comptes["par_trait"])],
            "non_rattaches": [{"trait": t, "requetes": n}
                              for t, n in par_frequence(comptes["non_rattaches"])],
        },
        "poids": {cid: poids[cid] for cid in sorted(poids)},
        "manques_par_frequence": manques,
        "rejeu": rejeu,
    }


ORIGINE = re.compile(r"^https://github\.com/([^/]+/[^/]+)/blob/[0-9a-f]+/([^/]*/?[^/]*)")


def concentration(corpus, combien=15):
    """D'ou vient la masse du corpus.

    Une etude qui ne publie pas ca laisse croire que ses 5 000 requetes sont
    5 000 usages independants. Elles ne le sont pas : un seul repertoire de
    tracks Rally (les requetes de Kibana sur des logs) en porte des centaines,
    et tout ce que cette application-la fait systematiquement herite de son
    poids.
    """
    compte = collections.Counter()
    for requete in corpus:
        m = ORIGINE.match(requete.get("ref") or "")
        compte[f"{m.group(1)}/{m.group(2)}" if m else "?"] += 1
    total = len(corpus) or 1
    return [{"origine": o, "requetes": n, "part": round(100.0 * n / total, 1)}
            for o, n in par_frequence(compte, combien)]


def distribution(resultats, clef):
    par = collections.defaultdict(collections.Counter)
    for res in resultats:
        par[res[clef] or "?"][res["verdict"]] += 1
        par[res[clef] or "?"]["total"] += 1
    return {k: dict(v) for k, v in sorted(par.items(), key=lambda kv: -kv[1]["total"])}


def erreurs_frequentes(resultats, combien=30):
    """Ce que ferrite repond quand il refuse une requete qu'ES accepte."""
    compte = collections.Counter()
    for res in resultats:
        rej = res.get("rejeu") or {}
        if rej.get("etat") == "refusee":
            compte[normalise_erreur(rej.get("ferrite_erreur", ""))] += 1
    return [{"erreur": e, "requetes": n} for e, n in par_frequence(compte, combien)]


NOMBRES = re.compile(r"\b\d+\b")


def normalise_erreur(message):
    """Deux refus qui ne different que par un nombre sont le meme refus. Ce qui
    est **entre crochets** est garde : c'est le nom du parametre refuse, donc
    tout ce qui distingue un manque d'un autre."""
    return NOMBRES.sub("N", message or "")[:160]


def accord(resultats):
    """Le croisement declare et le rejeu mesure disent-ils la meme chose ?

    C'est l'etalonnage : un desaccord n'est pas du bruit, c'est un defaut de
    `compat.yaml` — soit une capacite declaree servie qui ne l'est pas, soit un
    refus declare que ferrite accepte en fait.
    """
    matrice = collections.Counter()
    exemples = collections.defaultdict(list)
    for res in resultats:
        rej = res.get("rejeu")
        if not rej or rej["etat"] == "hors_mesure":
            continue
        clef = f"{res['verdict']}/{rej['etat']}"
        matrice[clef] += 1
        if res["verdict"] == SERVI and rej["etat"] != "servie" or \
           res["verdict"] != SERVI and rej["etat"] == "servie":
            if len(exemples[clef]) < 12:
                exemples[clef].append({"ref": res["ref"], "traits": res["traits"][:8],
                                       "erreur": rej.get("ferrite_erreur", "")})
    return {"matrice": dict(matrice), "desaccords": {k: v for k, v in exemples.items()}}


LIGNE_POIDS = re.compile(r"^(\s*)poids:\s*(.*)$")
LIGNE_ID = re.compile(r"^      - id: (\S+)\s*$")


def ecrit_poids(poids, chemin=COMPAT, verifie=False):
    """Ecrit (ou verifie) `poids:` dans compat.yaml, capacite par capacite.

    Une reecriture ligne a ligne, pas un round-trip YAML : ce fichier porte des
    commentaires qui valent son contenu, et aucune bibliotheque ne les rend a
    l'identique.
    """
    with open(chemin, encoding="utf-8") as f:
        lignes = f.readlines()
    courant, sorties, ecarts, vus = None, [], [], set()
    for ligne in lignes:
        m = LIGNE_ID.match(ligne)
        if m:
            courant = m.group(1)
        p = LIGNE_POIDS.match(ligne)
        if p and courant and courant in poids:
            vus.add(courant)
            valeur = poids[courant]
            texte = "null" if valeur is None else f"{valeur:.1f}"
            if p.group(2).strip() != texte:
                ecarts.append((courant, p.group(2).strip(), texte))
            ligne = f"{p.group(1)}poids: {texte}\n"
        sorties.append(ligne)
    manquants = set(poids) - vus
    if manquants:
        raise SystemExit(f"capacites sans ligne [poids] : {sorted(manquants)}")
    if verifie:
        return ecarts
    with open(chemin, "w", encoding="utf-8") as f:
        f.writelines(sorties)
    return ecarts


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default=CORPUS)
    ap.add_argument("--json", dest="sortie_json")
    ap.add_argument("--verdicts", default=VERDICTS)
    ap.add_argument("--rejoue", nargs=2, metavar=("FERRITE", "ES"))
    ap.add_argument("--poids", action="store_true", help="ecrit les poids dans compat.yaml")
    ap.add_argument("--verifie", action="store_true",
                    help="echoue si compat.yaml et la mesure divergent")
    ap.add_argument("--top", type=int, default=25)
    args = ap.parse_args()

    corpus = charge_corpus(args.corpus)
    croisement = Croisement()
    resultats = mesure(corpus, croisement)
    comptes = compte(resultats)
    poids = poids_mesures(comptes, croisement)
    journal_mappings = []
    compte_rejeu = (rejoue(corpus, resultats, *args.rejoue, journal=journal_mappings)
                    if args.rejoue else None)
    if compte_rejeu is not None:
        comptes["rejeu"] = compte_rejeu

    if args.verifie:
        ecarts = ecrit_poids(poids, verifie=True)
        if ecarts:
            for cid, avant, apres in ecarts:
                print(f"  {cid:<38} compat.yaml [{avant}] != mesure [{apres}]", file=sys.stderr)
            print(f"compat.yaml : {len(ecarts)} poids divergent du corpus "
                  f"(python3 tests/compat/ponderation.py --poids)", file=sys.stderr)
            return 1
        print(f"== poids : {len(poids)} capacites, compat.yaml conforme au corpus")
        return 0

    if args.poids:
        ecarts = ecrit_poids(poids)
        print(f"== {len(ecarts)} poids mis a jour dans compat.yaml")

    sources = json.load(open(SOURCES, encoding="utf-8")) if os.path.exists(SOURCES) else {}
    doc = rapport(corpus, resultats, comptes, croisement, poids, compte_rejeu, sources)
    if args.sortie_json:
        with open(args.sortie_json, "w", encoding="utf-8") as f:
            json.dump(doc, f, ensure_ascii=False, indent=1, sort_keys=False)
            f.write("\n")
    if args.sortie_json and compte_rejeu is not None:
        # Le detail par requete n'est ecrit que si le rejeu a eu lieu : sans
        # lui, ce fichier ne contiendrait que du recalculable, et le commiter
        # ferait perdre la seule chose qui ne se refait pas sans Docker.
        with open(args.verdicts, "w", encoding="utf-8") as f:
            for res in resultats:
                # `traits` et `capacites` ne sont pas ecrits : ils se
                # recalculent depuis le corpus sans rien de plus qu'un
                # interpreteur Python, la ou le `rejeu` demande un Docker et un
                # vrai Elasticsearch. On ne publie que ce qui ne se refait pas
                # tout seul — plus les manques, qui sont la lecture du verdict.
                f.write(json.dumps({c: v for c, v in res.items()
                                    if c not in ("traits", "capacites")},
                                   ensure_ascii=False, sort_keys=True) + "\n")
        print(f"== verdicts -> {args.verdicts}")
    if args.sortie_json:
        print(f"== rapport -> {args.sortie_json}")

    total = doc["totaux"]["requetes"]
    print(f"== {total} requetes, {doc['totaux']['servies']} servies entierement "
          f"({doc['totaux']['taux_servies']} %), "
          f"{doc['totaux']['refusees']} refusees, "
          f"{doc['totaux']['indeterminees']} indeterminees")
    if doc["rejeu"]:
        r = doc["rejeu"]
        print(f"== rejeu : {r['servies']}/{r['denominateur']} servies ({r['taux']} %), "
              f"{r['es_refuse_aussi']} hors mesure (ES refuse aussi), "
              f"{r['acceptees_par_ferrite_seul']} acceptees par ferrite seul")
    print("\n-- ce qui manque, par frequence d'usage")
    for manque in doc["manques_par_frequence"][:args.top]:
        print(f"  {manque['part']:>5.1f} %  {manque['requetes']:>5}  "
              f"{manque['capacite']:<32} {manque['nom'][:44]}")
    if doc["distribution"]["non_rattaches"]:
        print("\n-- traits qu'aucune capacite ne reclame (comptes contre nous)")
        for t in doc["distribution"]["non_rattaches"][:15]:
            print(f"  {t['requetes']:>5}  {t['trait']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
