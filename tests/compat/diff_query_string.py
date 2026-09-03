#!/usr/bin/env python3
"""Sonde : le mini-langage de `query_string` et de `simple_query_string`.

C'est la clause qu'envoie tout ce qui laisse quelqu'un **ecrire** sa requete —
la barre de Kibana, un panneau Grafana, un filtre « recherche avancee ». Son
contenu n'est pas du JSON : c'est une grammaire, celle du `QueryParser` classique
de Lucene, avec ses operateurs, ses parentheses, ses bornes et ses jokers.

Un parseur qui accepte une expression et l'interprete **autrement** qu'ES rend
silencieusement les mauvais documents. Ce fichier pose donc la meme expression
aux deux serveurs et compare ce qu'un client lit :

- le statut, le **type** d'erreur et sa **phrase** (celle du `root_cause`, la
  seule qu'un client officiel remonte) ;
- les documents rendus, **dans leur ordre**, et le total.

Les scores eux-memes ne sont pas compares : tantivy et Lucene ne calculent pas
les memes statistiques BM25 des qu'un document n'a pas le champ interroge (voir
`sonde_explain.py --ecart`). Un ecart qui ne permute que des documents auxquels
**ES lui-meme** donne le meme score n'en est donc pas un — c'est le predicat de
`diff_relevance.py`, repris ici tel quel.

    python3 tests/compat/diff_query_string.py [ferrite] [es]
    python3 tests/compat/diff_query_string.py --calibrer [es_a] [es_b]

`--calibrer` rejoue la batterie contre **deux** Elasticsearch : tant qu'elle n'y
est pas a zero, ce qu'elle dit de ferrite ne vaut rien. Et son rapport imprime la
**version** de chaque cible, parce qu'un etalonnage a deux serveurs de la meme
version prouve le determinisme, pas l'independance a la version (carte 41).
"""
import json
import sys
import urllib.error
import urllib.request

INDEX = "sonde-qs"
INDEX_NESTED = "sonde-qs-nested"


def http(base, method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        base + path, data=data, method=method,
        headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        corps = e.read()
        try:
            return e.code, json.loads(corps or b"{}")
        except json.JSONDecodeError:
            return e.code, {"raw": corps.decode(errors="replace")}


# ---------------------------------------------------------------------------
# Le corpus
# ---------------------------------------------------------------------------
#
# Tous les documents portent **tous** les champs. Ce n'est pas de la coquetterie :
# c'est la seule forme sous laquelle les deux moteurs calculent le meme `N` et le
# meme `avgdl`, donc les memes scores, donc le meme ordre (mesure de la carte 43).
# Le seul document creux est `d9`, et il est la exprofes pour `_exists_`.

MAPPING = {
    "settings": {"index": {"number_of_shards": 1}},
    "mappings": {"properties": {
        "titre": {"type": "text", "fields": {"raw": {"type": "keyword"}}},
        "corps": {"type": "text"},
        "fr": {"type": "text", "analyzer": "french"},
        "tag": {"type": "keyword"},
        "n": {"type": "long"},
        "f": {"type": "double"},
        "d": {"type": "date"},
        "b": {"type": "boolean"},
        "cache": {"type": "keyword", "index": False},
        "obj": {"properties": {"sub": {"type": "text"},
                               "k": {"type": "keyword"}}},
    }},
}

DOCS = {
    "d1": {"titre": "le chat noir", "corps": "un chat dort ici",
           "fr": "les chattes noires", "tag": "Chat", "n": 5, "f": 1.5,
           "d": "2026-03-15", "b": True, "cache": "chat",
           "obj": {"sub": "chat", "k": "Chat"}},
    "d2": {"titre": "le chien blanc", "corps": "un chien court la",
           "fr": "les chiens blancs", "tag": "chien", "n": 12, "f": 2.5,
           "d": "2026-05-01", "b": False, "cache": "chien",
           "obj": {"sub": "chien", "k": "chien"}},
    "d3": {"titre": "chat et chien", "corps": "le chat suit le chien",
           "fr": "chatte et chien", "tag": "chat-chien", "n": 0, "f": 0.0,
           "d": "2025-12-31", "b": True, "cache": "chat-chien",
           "obj": {"sub": "chat chien", "k": "chat chien"}},
    "d4": {"titre": "oiseau bleu", "corps": "un oiseau vole haut",
           "fr": "les oiseaux bleus", "tag": "oiseau", "n": 100, "f": 12.75,
           "d": "2024-06-01", "b": False, "cache": "oiseau",
           "obj": {"sub": "oiseau", "k": "oiseau"}},
    "d5": {"titre": "chat-huant", "corps": "le chat-huant hulule",
           "fr": "chats-huants", "tag": "chat_huant", "n": -3, "f": -1.25,
           "d": "2026-01-01", "b": True, "cache": "chat-huant",
           "obj": {"sub": "chat huant", "k": "chat-huant"}},
    "d6": {"titre": "l'ascension du mont", "corps": "une ascension rude",
           "fr": "l'ascension du mont", "tag": "mont", "n": 42, "f": 3.5,
           "d": "2026-02-29" if False else "2026-02-28", "b": False,
           "cache": "mont", "obj": {"sub": "mont", "k": "mont"}},
    "d7": {"titre": "a b c", "corps": "a b c d", "fr": "a b c",
           "tag": "abc", "n": 1, "f": 1.0, "d": "2026-04-01", "b": True,
           "cache": "abc", "obj": {"sub": "a b", "k": "a b"}},
    "d8": {"titre": "AND OR NOT", "corps": "and or not to be",
           "fr": "and or not", "tag": "AND", "n": 7, "f": 7.0,
           "d": "2026-07-04", "b": False, "cache": "AND",
           "obj": {"sub": "and", "k": "AND"}},
    "d9": {"titre": "seul le titre"},
    "d10": {"titre": "prix 100 euros", "corps": "cent euros tout rond",
            "fr": "cent euros", "tag": "100", "n": 100, "f": 100.0,
            "d": "2026-06-15", "b": True, "cache": "100",
            "obj": {"sub": "100", "k": "100"}},
    "d11": {"titre": "c++ et c#", "corps": "des langages c++ c#",
            "fr": "langages", "tag": "c++", "n": 3, "f": 3.25,
            "d": "2026-08-08", "b": False, "cache": "c++",
            "obj": {"sub": "c", "k": "c++"}},
    "d12": {"titre": "chat noir chat blanc", "corps": "deux chats",
            "fr": "deux chattes", "tag": "chat", "n": 5, "f": 5.0,
            "d": "2026-03-15", "b": True, "cache": "chat",
            "obj": {"sub": "chat", "k": "chat"}},
}

MAPPING_NESTED = {
    "settings": {"index": {"number_of_shards": 1}},
    "mappings": {"properties": {
        "titre": {"type": "text"},
        "lignes": {"type": "nested", "properties": {"x": {"type": "text"}}},
    }},
}
DOCS_NESTED = {
    "n1": {"titre": "commande", "lignes": [{"x": "chat"}, {"x": "chien"}]},
    "n2": {"titre": "chat", "lignes": [{"x": "oiseau"}]},
}


def bulk(base, index, docs):
    lignes = []
    for id_, doc in docs.items():
        lignes.append(json.dumps({"index": {"_index": index, "_id": id_}}))
        lignes.append(json.dumps(doc))
    corps = "\n".join(lignes) + "\n"
    req = urllib.request.Request(
        base + "/_bulk?refresh=true", data=corps.encode(), method="POST",
        headers={"Content-Type": "application/x-ndjson"})
    urllib.request.urlopen(req).read()


def prepare(base):
    for index, mapping, docs in ((INDEX, MAPPING, DOCS),
                                 (INDEX_NESTED, MAPPING_NESTED, DOCS_NESTED)):
        http(base, "DELETE", f"/{index}")
        st, corps = http(base, "PUT", f"/{index}", mapping)
        if st != 200:
            raise SystemExit(f"# creation de [{index}] refusee sur {base} : "
                             f"{json.dumps(corps)[:400]}")
        bulk(base, index, docs)


# ---------------------------------------------------------------------------
# La batterie
# ---------------------------------------------------------------------------
#
# Chaque cas est `(libelle, clause, corps de la clause, index)`. Le corps porte
# toujours `query` ; le reste sont les parametres a mesurer.


def q(expr, **kw):
    return dict(query=expr, **kw)


def cas_query_string():
    """Les expressions, avec un `default_field` explicite quand le sujet n'est
    pas l'expansion des champs."""
    T = {"default_field": "titre"}
    out = []

    def a(expr, **kw):
        out.append((f"qs {expr!r}" + (f" {kw}" if kw else ""), q(expr, **kw)))

    # -- un terme, un champ ------------------------------------------------
    for expr in ["chat", "CHAT", "chien", "chat chien", "chat-huant",
                 "ascension", "l'ascension", "inexistant", "a", "100"]:
        a(expr, **T)
    for expr in ["titre:chat", "titre:CHAT", "corps:chat", "tag:Chat",
                 "tag:chat", "titre.raw:chat", "obj.sub:chat", "obj.k:Chat",
                 "n:5", "n:100", "f:1.5", "b:true", "b:false",
                 "d:2026-03-15", "cache:chat", "absent:chat",
                 "titre:chat corps:chien", "fr:chattes", "fr:chatte"]:
        a(expr)
        a(expr, **T)
    # Le champ nomme l'emporte-t-il sur `default_field` ? Et sur `fields` ?
    a("corps:chat", default_field="titre")
    a("corps:chat", fields=["titre"])

    # -- operateurs et precedence ------------------------------------------
    for expr in [
        "chat AND chien", "chat OR chien", "chat && chien", "chat || chien",
        "NOT chat", "!chat", "chat AND NOT chien", "chat NOT chien",
        "chat OR chien AND oiseau", "chat AND chien OR oiseau",
        "+chat", "-chat", "+chat -chien", "+chat +chien", "chat +chien",
        "+chat chien", "-chat -chien", "chat chien oiseau",
        "titre:chat AND corps:chien", "titre:chat OR n:5",
        "NOT titre:chat", "!titre:chat", "+titre:chat -tag:chien",
        "chat AND (chien OR oiseau)", "(chat OR chien) AND noir",
        "((chat))", "(chat)", "titre:(chat OR chien)", "titre:(chat chien)",
        "titre:(+chat -chien)", "titre:(chat AND chien)",
        "and", "or", "not", "AND OR", "chat and chien",
    ]:
        a(expr, **T)
    for op in ["AND", "OR"]:
        a(f"chat {op} chien", default_field="titre", default_operator="AND")
        a(f"chat {op} chien", default_field="titre", default_operator="OR")
    a("chat chien", default_field="titre", default_operator="AND")
    a("chat chien", default_field="titre", default_operator="and")
    a("chat chien", default_field="titre", default_operator="nawak")

    # -- phrases -----------------------------------------------------------
    for expr in ['"le chat"', '"chat noir"', '"noir chat"', '"chat"',
                 'titre:"le chat"', 'corps:"un chat dort"',
                 'tag:"chat-chien"', '"chat" AND "chien"',
                 '"le chat" OR "le chien"', '""', 'titre:""',
                 '"a b"~1', '"a c"~2', '"le chat"~0']:
        a(expr, **T)

    # -- bornes ------------------------------------------------------------
    for expr in [
        "n:[1 TO 10]", "n:{1 TO 10}", "n:[1 TO 10}", "n:{1 TO 10]",
        "n:[* TO 10]", "n:[1 TO *]", "n:[* TO *]", "n:>5", "n:>=5",
        "n:<5", "n:<=5", "n:[-5 TO 0]", "n:[0 TO 0]",
        "f:[1 TO 3]", "f:[1.5 TO 1.5]", "f:>3.0",
        "d:[2026-01-01 TO 2026-04-01]", "d:{2026-01-01 TO 2026-04-01}",
        "d:>2026-04-01", "d:<2026-01-01", "d:[2026 TO 2027]",
        "titre:[a TO c]", "tag:[a TO d]", "tag:{a TO d}",
        "tag:[Chat TO chien]", "cache:[a TO d]",
        "n:[abc TO 10]", "d:[nawak TO 2026]",
        "n:[1 TO 10] AND titre:chat", "n:>5 OR n:<0",
        'tag:["chat" TO "chien"]',
    ]:
        a(expr)
        if ":" in expr and expr.split(":")[0] in ("titre", "tag"):
            a(expr, **T)

    # -- jokers, prefixes, regexp ------------------------------------------
    for expr in [
        "cha*", "CHA*", "*hat", "ch?t", "c?a*", "*", "chat*", "*chat*",
        "titre:cha*", "titre:CHA*", "titre:*hat", "titre:ch?t", "titre:*",
        "tag:Ch*", "tag:ch*", "tag:*hat", "tag:*",
        "fr:chattes*", "fr:CHATTES*", "obj.k:Ch*",
        "n:1*", "cache:cha*",
        "/ch.t/", "titre:/ch.t/", "titre:/cha./", "tag:/[Cc]hat/",
        "titre:/ch(a|i)t/", "titre:/ch\\dt/",
        "_exists_:titre", "_exists_:corps", "_exists_:absent",
        "_exists_:n", "_exists_:cache",
    ]:
        a(expr, **T)
    a("titre:CHA*", default_field="titre", analyze_wildcard=True)
    a("fr:CHATTES*", default_field="fr", analyze_wildcard=True)
    a("fr:chattes*", default_field="fr", analyze_wildcard=True)
    a("tag:CH*", analyze_wildcard=True)
    a("cha*", default_field="titre", analyze_wildcard=True)
    a("*hat", default_field="titre", allow_leading_wildcard=False)
    a("?hat", default_field="titre", allow_leading_wildcard=False)
    a("cha*", default_field="titre", allow_leading_wildcard=False)

    # -- flou --------------------------------------------------------------
    for expr in ["chat~", "chat~1", "chat~2", "chat~0", "chat~5", "chot~",
                 "titre:chot~1", "chien~2", "titre:chat~1 AND corps:chien",
                 "chat~1.5"]:
        a(expr, **T)
    a("chot~", default_field="titre", fuzziness=2)
    a("chot~", default_field="titre", fuzziness="AUTO")
    a("chot~", default_field="titre", fuzziness="0")
    a("chot~", default_field="titre", fuzzy_transpositions=False)
    a("chta~", default_field="titre", fuzzy_transpositions=False)
    a("chta~", default_field="titre", fuzzy_transpositions=True)
    a("chot~", default_field="titre", fuzzy_prefix_length=2)
    a("cxot~", default_field="titre", fuzzy_prefix_length=2)
    a("cha*", default_field="titre", fuzzy_max_expansions=1)

    # -- ponderation -------------------------------------------------------
    for expr in ["chat^2", "chat^0.5", "chat^0", "(chat OR chien)^3",
                 '"le chat"^2', "titre:chat^2 OR corps:chien^3",
                 "chat^-1", "chat^x", "chat^"]:
        a(expr, **T)
    a("chat", default_field="titre", boost=2)
    a("chat", default_field="titre", boost=0)

    # -- echappement et caracteres speciaux --------------------------------
    for expr in [
        "c\\+\\+", "titre:c\\+\\+", "tag:c++", "tag:c\\+\\+",
        "chat\\-huant", "titre:chat\\-huant", "\\*", "\\?",
        "titre:\\*", "a\\ b", "titre:a\\ b", "l\\'ascension",
        "titre:(a\\ b)", "\\AND", "chat\\ chien",
    ]:
        a(expr, **T)

    # -- bords et refus ----------------------------------------------------
    for expr in [
        "", "   ", "titre:", ":", ":chat", "AND chat", "chat AND",
        "OR chat", "NOT", "titre:\"chat", "(chat", "chat)", "()",
        "(chat OR)", "[1 TO", "n:[1 TO", "n:1 TO 10]", "~", "^2",
        "titre:^2", "+", "-", "+-chat", "\\", "a b)c",
        "titre:{a TO b]", "*:*", "titre:chat~~", "((((chat))))",
    ]:
        a(expr, **T)

    # -- une clause qui ne produit **aucun** terme ---------------------------
    # Lucene ne l'ajoute pas au booleen (`if (q == null) return;`) : elle ne
    # compte donc ni dans les clauses obligatoires ni dans le denominateur d'un
    # `minimum_should_match`. La traiter comme « ne correspond a rien » rendrait
    # zero document la ou ES en rend, en 200.
    for expr in ["titre:...", "...", "chat AND ...", "... AND chat",
                 "chat OR ...", "+chat +...", "+... +chat", "chat ...",
                 "-... chat", "NOT ...", "titre:(chat AND ...)",
                 "chat AND ... AND chien", "\"...\"", "chat \"...\"",
                 "...^2", "(...)", "(... OR chat)"]:
        a(expr, **T)
    a("chat chien ...", default_field="titre", minimum_should_match="2")
    a("chat ... ...", default_field="titre", minimum_should_match="2")
    a("... ...", default_field="titre", minimum_should_match="1")

    # -- deux mots separes par un blanc font **une** clause -----------------
    # `split_on_whitespace: false`, figé chez ES depuis la 7.0. Invisible sur un
    # champ `text` (l'analyzer redecoupe), decisif ailleurs : sur un `keyword`,
    # `chat noir` cherche le terme `chat noir` ; sur un numerique, la chaine
    # entiere est illisible. Trouve par le fuzzer, pas par les questions ecrites
    # a la main. Le groupe s'arrete devant `AND`, `OR`, `^`, `~` et `:` —
    # chacun a son cas ici, parce qu'aucune de ces cinq frontieres ne se devine.
    for expr in [
        "chat noir", "chat noir blanc", "chat noir AND chien",
        "chat noir OR chien", "chat noir NOT chien", "chat noir +chien",
        "chat noir -chien", "chat noir^2", "chat noir~", "chat noir tag:chat",
        "+chat noir", "chat +noir", "-chat noir", "chat^2 noir",
        "titre:chat noir", "chat titre:noir", "(chat noir)", "chat \"noir\"",
        "chat noir*", "chat noir chien AND oiseau", "chat   noir",
        "a b c d", "chat noir 5",
    ]:
        a(expr)
        a(expr, **T)
    a("chat noir", fields=["titre", "tag"])
    a("chat noir", fields=["tag"])
    a("chat noir", fields=["n"])
    a("chat noir", default_field="tag")
    a("chat noir", default_field="titre", default_operator="AND")
    a("chat noir chien", default_field="titre", minimum_should_match="2")
    a("chat noir chien", fields=["titre", "corps"], minimum_should_match="2")
    a("chat noir chien", default_field="titre", minimum_should_match="2",
      default_operator="AND")

    # -- echappements et noms de champ --------------------------------------
    for expr in [
        "obj.sub:chat", "obj\\.sub:chat", "titre\\:chat", "\\titre:chat",
        "titre:cha\\*", "titre:cha\\?t", "titre:\\-chat", "titre:a\\&\\&b",
        "titre:(chat)^2", "titre:chat^2^3", "((chat OR chien) AND noir)^2",
        "titre:chat AND (corps:chien OR (tag:oiseau AND n:100))",
        "chat && chien || oiseau", "!chat && chien",
        "NOT NOT chat", "chat AND NOT NOT chien",
    ]:
        a(expr, **T)

    return [(lib, "query_string", corps, INDEX) for lib, corps in out]


def cas_query_string_champs():
    """L'expansion des champs : `default_field`, `fields`, et le `*` par
    defaut — dont ES tire aussi la valeur par defaut de `lenient`."""
    out = []

    def a(expr, **kw):
        out.append((f"qs/champs {expr!r} {kw}", q(expr, **kw)))

    for expr in ["chat", "chat chien", "titre:chat", "n:5", "100"]:
        a(expr)                                   # default_field = "*"
        a(expr, default_field="titre")
        a(expr, fields=["titre"])
        a(expr, fields=["titre", "corps"])
        a(expr, fields=["titre^3", "corps"])
        a(expr, fields=["*"])
        a(expr, fields=["t*"])
        a(expr, fields=["titre", "absent"])
        a(expr, fields=["absent"])
    a("chat", fields=[])
    a("chat", default_field="absent")
    a("chat", fields=["titre", "corps"], tie_breaker=0.3)
    a("chat", fields=["titre", "corps"], tie_breaker=1.0)
    a("chat", fields=["titre", "corps"], type="most_fields")
    a("chat", fields=["titre", "corps"], type="best_fields")
    a("chat", fields=["titre", "corps"], type="phrase")
    a("chat", fields=["titre", "corps"], type="cross_fields")
    a("chat", fields=["titre", "corps"], type="bool_prefix")
    a("chat", fields=["titre", "corps"], type="nawak")

    # `lenient` : ce que le champ ne sait pas lire.
    for kw in ({}, {"default_field": "titre"}, {"fields": ["titre"]},
               {"fields": ["*"]}, {"default_field": "*"}):
        a("n:abc", **kw)
        a("d:nawak", **kw)
        a("b:oui", **kw)
    a("n:abc", default_field="titre", lenient=True)
    a("n:abc", default_field="titre", lenient=False)
    a("n:abc", lenient=False)
    a("n:abc", fields=["titre", "n"], lenient=True)
    a("d:nawak", default_field="titre", lenient=True)
    a("b:oui", default_field="titre", lenient=True)

    # `minimum_should_match`
    for msm in ["2", 2, "50%", "75%", "-1", "100%", "3", "0"]:
        a("chat chien oiseau", default_field="titre",
          minimum_should_match=msm)
        a("chat OR chien OR oiseau", fields=["titre", "corps"],
          minimum_should_match=msm)
    a("chat AND chien", default_field="titre", minimum_should_match="2")

    # Les parametres restants, un par un.
    a("chat", default_field="titre", analyzer="keyword")
    a("chat", default_field="titre", analyzer="standard")
    a("CHAT", default_field="titre", analyzer="keyword")
    a("chat", default_field="titre", quote_analyzer="keyword")
    a('"le chat"', default_field="titre", quote_field_suffix=".raw")
    a("chat", default_field="titre", time_zone="Europe/Paris")
    a("d:>2026-01-01", time_zone="Europe/Paris")
    a("chat", default_field="titre", rewrite="constant_score")
    a("chat", default_field="titre", escape=True)
    a("c++", default_field="titre", escape=True)
    a("chat", default_field="titre", phrase_slop=2)
    a('"le chat"', default_field="titre", phrase_slop=2)
    a("chat", default_field="titre", auto_generate_synonyms_phrase_query=False)
    a("chat", default_field="titre", auto_generate_synonyms_phrase_query=True)
    a("chat", default_field="titre", enable_position_increments=False)
    a("chat", default_field="titre", max_determinized_states=100)
    a("chat", default_field="titre", fuzzy_rewrite="constant_score")
    a("chat", default_field="titre", _name="nomme")
    a("chat", default_field="titre", nawak=1)
    out.append(("qs/champs sans [query]", {"default_field": "titre"}))

    return [(lib, "query_string", corps, INDEX) for lib, corps in out]


def cas_nested():
    """Un sous-champ de `nested` cite depuis la racine : ES rend zero document
    **en silence** (la clause vise les documents caches, que le filtre de
    parent ecarte ensuite). C'est le seul endroit ou ce fichier attend un refus
    de ferrite plutot qu'une egalite."""
    out = [
        ("qs/nested 'lignes.x:chat'", q("lignes.x:chat")),
        ("qs/nested 'lignes.x:chat' df=titre",
         q("lignes.x:chat", default_field="titre")),
        ("qs/nested 'chat' (expansion *)", q("chat")),
        ("qs/nested 'chat' fields=*", q("chat", fields=["*"])),
        ("qs/nested 'chat' df=titre", q("chat", default_field="titre")),
        ("qs/nested '_exists_:lignes.x'", q("_exists_:lignes.x")),
    ]
    return [(lib, "query_string", corps, INDEX_NESTED) for lib, corps in out]


def cas_simple_query_string():
    """`simple_query_string` : le meme langage sans les operateurs risques, et
    surtout **sans jamais lever**. Un caractere mal place y est du texte."""
    out = []

    def a(expr, **kw):
        out.append((f"sqs {expr!r}" + (f" {kw}" if kw else ""), q(expr, **kw)))

    F = {"fields": ["titre"]}
    for expr in [
        "chat", "CHAT", "chat chien", "chat + chien", "chat | chien",
        "-chat", "+chat -chien", "chat+chien", "chat|chien",
        '"le chat"', '"chat noir"', '"le chat"~1', "cha*", "chat*",
        "*chat", "chat~", "chat~1", "chat~2", "chat~5", "ch?t",
        "(chat | chien) + noir", "(chat", "chat)", "()",
        "chat AND chien", "AND", "chat OR chien", "NOT chat",
        "titre:chat", "n:5", "chat~~", "+", "-", "|", "~", "",
        "   ", "\\", "chat\\-huant", "chat-huant", "c++", "c\\+\\+",
        "chat + + chien", "((chat))", "chat +", "+ chat",
        '"chat', 'chat"', "[1 TO 10]", "n:[1 TO 10]", "*",
    ]:
        a(expr, **F)
    for expr in ["chat", "chat chien", "n:5", "100", "chat + chien"]:
        a(expr)
        a(expr, fields=["titre", "corps"])
        a(expr, fields=["titre^3", "corps"])
        a(expr, fields=["*"])
        a(expr, fields=["t*"])
        a(expr, fields=["absent"])
    a("chat chien", fields=["titre"], default_operator="AND")
    a("chat chien", fields=["titre"], default_operator="and")
    a("chat chien", fields=["titre"], default_operator="nawak")
    a("chat chien oiseau", fields=["titre"], minimum_should_match="2")
    a("chat chien oiseau", fields=["titre"], minimum_should_match="50%")
    a("chat", fields=["titre"], boost=2)
    a("chat", fields=["titre"], lenient=True)
    a("chat", fields=["n"], lenient=True)
    a("chat", fields=["n"], lenient=False)
    a("chat", fields=["n"])
    a("chat", fields=["n", "titre"])
    a("5", fields=["n"])
    a("chat", fields=["titre"], analyzer="keyword")
    a("CHAT", fields=["titre"], analyzer="keyword")
    a("cha*", fields=["titre"], analyze_wildcard=True)
    a("CHA*", fields=["titre"], analyze_wildcard=True)
    a("CHA*", fields=["titre"])
    a("CHATTES*", fields=["fr"], analyze_wildcard=True)
    a("CHATTES*", fields=["fr"])
    a('"le chat"', fields=["titre"], quote_field_suffix=".raw")
    a("chat", fields=["titre"], auto_generate_synonyms_phrase_query=False)
    a("chat", fields=["titre"], fuzzy_prefix_length=2)
    a("chot~", fields=["titre"], fuzzy_prefix_length=2)
    a("chot~", fields=["titre"], fuzzy_transpositions=False)
    a("chot~", fields=["titre"], fuzzy_max_expansions=1)
    a("chat", fields=["titre"], _name="nomme")
    a("chat", fields=["titre"], nawak=1)
    # Les `flags` : le sous-ensemble de la grammaire qui reste actif.
    for flags in ["ALL", "NONE", "AND", "OR", "NOT", "PREFIX", "PHRASE",
                  "PRECEDENCE", "ESCAPE", "WHITESPACE", "FUZZY", "NEAR",
                  "SLOP", "OR|AND|PREFIX", "ALL^NOT", "nawak"]:
        a("chat + chien", fields=["titre"], flags=flags)
        a("cha*", fields=["titre"], flags=flags)
        a("chat | chien", fields=["titre"], flags=flags)
    # Les bords que le parseur de `simple_query_string` doit tenir sans jamais
    # lever : operateurs colles, groupes non fermes, negations enchainees, et
    # les mots dont l'analyzer ne tire aucun terme.
    for expr in [
        "chat + -chien", "-chat + chien", "-chat -chien", "--chat",
        "chat | -chien", "(chat + chien) | oiseau", "((chat | chien) + noir)",
        "(chat + (chien | oiseau))", "(((chat", "chat))",
        "( chat | chien ) + ( noir | blanc )", "-(chat | chien)",
        "+(chat)", "\"chat noir\" + chien", "\"chat\" | \"chien\"",
        "chat*| chien", "cha*+chien", "\"le chat\"~", "\"le chat\"~x",
        "chat~x", "chat~-1", "*", "**", "a*b", "chat* chien*",
        "...", "chat ...", "... + chat", "chat + ...",
        "\\+chat", "\\-chat", "\\|chat", "\\(chat\\)", "chat\\ chien",
        "titre\\:chat", "l'ascension", "\"l'ascension\"",
        "chat AND NOT chien", "100", "chat noir",
    ]:
        a(expr, **F)
    # L'arbre de `simple_query_string` : binaire, construit de gauche a droite,
    # et un operateur qui repete celui du sommet l'**allonge** au lieu de
    # l'emboiter. `chat noir + chien` y vaut `(chat OU noir) ET chien`, pas
    # trois clauses a plat — trouve par le fuzzer.
    for expr in [
        "chat + noir", "chat + noir + chien", "chat noir + chien",
        "chat + noir chien", "chat | noir + chien", "chat + noir | chien",
        "chat + NOT -noir", "chat + noir -chien", "-chat + noir",
        "chat + -noir", "NOT -chat", "chat -noir", "chat + + noir",
        "chat noir + chien oiseau", "(chat + noir) | chien",
        "chat | noir | chien", "chat noir chien",
    ]:
        a(expr, **F)
    # L'echappement sur un `keyword`, ou il survit a l'analyse : un `\` en fin
    # d'entree n'echappe rien et disparait, un `*` echappe n'est pas un prefixe.
    for expr in ["chien\\", "chien\\*", "chien*", "chi\\en", "chien\\\\",
                 "chien\\ x", "\\*", "chat\\-chien", "chat\\ chien"]:
        a(expr, fields=["tag"])
        a(expr, fields=["titre"])
    a("chat noir + chien", fields=["titre"], minimum_should_match="2")
    a("chat noir chien", fields=["titre"], minimum_should_match="2")
    a("chat-noir", fields=["titre"], default_operator="AND")
    a("chat noir", fields=["titre"], default_operator="AND")
    a("chat ...", fields=["titre"], default_operator="AND")
    a("chat chien ...", fields=["titre"], minimum_should_match="2")
    out.append(("sqs sans [query]", {"fields": ["titre"]}))
    return [(lib, "simple_query_string", corps, INDEX) for lib, corps in out]


def batterie():
    return (cas_query_string() + cas_query_string_champs()
            + cas_nested() + cas_simple_query_string())


# ---------------------------------------------------------------------------
# La mesure
# ---------------------------------------------------------------------------


def interroge(base, clause, corps, index):
    """Ce qu'un client lit : le statut et la phrase du refus, ou les documents
    **dans leur ordre** avec le total.

    La phrase comparee est celle du `root_cause` : c'est celle que les clients
    officiels remontent dans leur exception, et la seule qu'une application
    voit. La chaine JavaCC que Lucene range sous `caused_by` n'y est pas (voir
    REFUS_ASSUMES)."""
    st, body = http(base, "POST", f"/{index}/_search",
                    {"size": 30, "_source": False, "query": {clause: corps}})
    if st != 200:
        err = body.get("error", {})
        cause = (err.get("root_cause") or [err])[0]
        return {"statut": st, "type": cause.get("type"),
                "phrase": cause.get("reason")}
    return {"statut": 200,
            "ids": [h["_id"] for h in body["hits"]["hits"]],
            "scores": [h.get("_score") for h in body["hits"]["hits"]],
            "total": body["hits"]["total"]["value"]}


def comparable(vu):
    """Ce qui doit coincider. Les scores en sont exclus : les deux moteurs ne
    calculent pas le meme `N` ni le meme `avgdl` des qu'un document n'a pas le
    champ interroge (`d9` n'en a qu'un)."""
    if vu["statut"] != 200:
        return json.dumps(vu, sort_keys=True)
    return json.dumps({"ids": vu["ids"], "total": vu["total"]})


def meme_ordre_aux_ex_aequo(gauche, droite):
    """Un ecart qui ne permute que des documents auxquels la **droite** (ES hors
    `--calibrer`) donne le meme score n'est pas un ecart : c'est le predicat de
    `diff_relevance.py`."""
    if gauche["statut"] != 200 or droite["statut"] != 200:
        return False
    if sorted(gauche["ids"]) != sorted(droite["ids"]):
        return False
    if gauche["total"] != droite["total"]:
        return False
    # Les groupes d'ex aequo de la droite, dans l'ordre.
    groupes, courant, score = [], [], None
    for id_, sc in zip(droite["ids"], droite["scores"]):
        if courant and sc != score:
            groupes.append(set(courant))
            courant = []
        courant.append(id_)
        score = sc
    if courant:
        groupes.append(set(courant))
    reste = list(gauche["ids"])
    for groupe in groupes:
        tete, reste = reste[:len(groupe)], reste[len(groupe):]
        if set(tete) != groupe:
            return False
    return True


# Les ecarts assumes, chacun avec sa raison **et** son predicat. Un libelle
# n'entre ici que si le predicat verifie que ferrite refuse explicitement : une
# entree qui ne verifierait que « les deux repondent differemment » serait un
# denominateur qu'on ecrit soi-meme.
REFUS_ASSUMES = {
    # La grammaire refusee, en la nommant.
    "slop": ("refus",
             "la proximite d'une phrase (`\"a b\"~n`) demande le `slop` de "
             "`match_phrase`, refuse dans tout ce depot : tantivy et Lucene ne "
             "comptent pas les deplacements pareil au-dela de deux termes, et "
             "l'accepter rendrait **moins** de documents qu'ES sans que rien "
             "ne le signale"),
    "nested": ("refus",
               "un sous-champ de `nested` cite depuis la racine : ES rend zero "
               "document en silence (la clause vise les documents caches), "
               "ferrite le refuse en le nommant — c'est la regle du depot, et "
               "elle est plus bruyante que celle d'ES"),
    "parametre": ("refus",
                  "un parametre qu'ES sert et que ferrite ne reproduit pas : "
                  "il est refuse en le nommant, jamais accepte et ignore"),
    "borne_texte": ("refus",
                    "`range` sur un champ [text] : refuse dans tout ce depot "
                    "(un intervalle de **termes** analyses n'est presque "
                    "jamais ce que le client croit demander), donc refuse ici "
                    "aussi plutot que servi a moitie"),
    "score_flou": ("ordre",
                   "le score d'un `fuzzy` : tantivy le rend **constant**, "
                   "Lucene pondere chaque terme par sa distance d'edition. "
                   "Divergence deja declaree dans `docs/compat.md` — les "
                   "documents rendus sont les memes, leur ordre non"),
    "phrase": ("phrase",
               "les deux serveurs refusent, avec le meme statut et le meme "
               "type ; seule la phrase differe — celle d'ES range le detail "
               "JavaCC sous `caused_by`, que ferrite ne reproduit pas"),
}


# Les champs `text` du corpus : une borne posee sur l'un d'eux est un refus
# assume, et cette liste est ce qui empeche le predicat de s'elargir tout seul.
TEXTE = ("titre", "corps", "fr", "obj.sub")


def classe_assumee(libelle, corps):
    """Quel ecart assume ce cas peut-il invoquer ? Rendu par la **forme** du
    cas, pas par une liste de libelles : une liste se remplirait toute seule."""
    expr = corps.get("query", "")
    if isinstance(expr, str) and '"' in expr and "~" in expr.split('"')[-1]:
        return "slop"
    if corps.get("phrase_slop"):
        return "slop"
    if "lignes.x" in str(expr) or libelle.startswith("qs/nested"):
        return "nested"
    for p in ("quote_field_suffix", "quote_analyzer", "analyzer", "time_zone",
              "rewrite", "escape", "max_determinized_states", "fuzzy_rewrite",
              "enable_position_increments", "fuzzy_prefix_length",
              "fuzzy_max_expansions"):
        if p in corps:
            return "parametre"
    if corps.get("type") in ("cross_fields", "bool_prefix", "phrase",
                             "phrase_prefix"):
        return "parametre"
    if corps.get("clause") == "simple_query_string" and "analyze_wildcard" in corps:
        return "parametre"
    if libelle.startswith("sqs ") and "analyze_wildcard" in corps:
        return "parametre"
    # Une borne posee sur un champ `text` : `champ:[a TO c]`, `champ:>a`.
    if isinstance(expr, str):
        for champ in TEXTE:
            if f"{champ}:[" in expr or f"{champ}:{{" in expr \
                    or f"{champ}:>" in expr or f"{champ}:<" in expr:
                return "borne_texte"
        # Un flou : `~` colle a un mot, ou le parametre qui le regle.
        mots = expr.replace("(", " ").replace(")", " ").split()
        if any("~" in m and not m.startswith('"') for m in mots) \
                or "fuzziness" in corps or "fuzzy_transpositions" in corps:
            return "score_flou"
    return "phrase"


def refuse(vu):
    return vu["statut"] != 200


def assume(libelle, corps, gauche, droite):
    classe = classe_assumee(libelle, corps)
    if classe == "phrase":
        # Les deux refusent, meme statut, meme type : seule la phrase differe.
        return (refuse(gauche) and refuse(droite)
                and gauche["statut"] == droite["statut"]
                and gauche["type"] == droite["type"])
    if classe == "score_flou":
        # Les memes documents et le meme total : seul l'ordre change, et il ne
        # change que parce que les scores de ferrite sont **tous egaux**. Un
        # document en plus ou en moins n'entre pas ici.
        return (not refuse(gauche) and not refuse(droite)
                and sorted(gauche["ids"]) == sorted(droite["ids"])
                and gauche["total"] == droite["total"]
                and len(set(gauche["scores"])) <= 1)
    # Les autres classes exigent que **ferrite** refuse explicitement. Rendre un
    # resultat en silence n'est jamais un ecart assume.
    return refuse(gauche)


def abrege(vu):
    s = json.dumps(vu, ensure_ascii=False, sort_keys=True)
    return s if len(s) <= 160 else s[:157] + "..."


def version(base):
    _, corps = http(base, "GET", "/")
    v = corps.get("version", {})
    return f"{corps.get('tagline', '?')[:24]} {v.get('number', '?')}"


def main():
    argv = [a for a in sys.argv[1:] if not a.startswith("--")]
    calibrer = "--calibrer" in sys.argv
    filtre = next((a.split("=", 1)[1] for a in sys.argv
                   if a.startswith("--filtre=")), None)
    gauche = argv[0] if argv else ("http://localhost:9201" if calibrer
                                   else "http://localhost:9200")
    droite = argv[1] if len(argv) > 1 else ("http://localhost:9202" if calibrer
                                            else "http://localhost:9201")
    cibles = [("es_a" if calibrer else "ferrite", gauche),
              ("es_b" if calibrer else "es", droite)]
    for nom, base in cibles:
        try:
            http(base, "GET", "/")
        except Exception as e:  # noqa: BLE001
            print(f"# {nom} indisponible ({base}) : {e}")
            print("# une sonde differentielle qui ne compare rien ne rend pas "
                  "de verdict : arret.")
            return 2
    # La version de chaque cible est **imprimee** : un etalonnage a deux
    # serveurs de la meme version prouve le determinisme, pas l'independance a
    # la version majeure (carte 41).
    for nom, base in cibles:
        print(f"# {nom} = {base} : {version(base)}")
    for _, base in cibles:
        prepare(base)

    ecarts = assumes = total = 0
    par_classe = {}
    for libelle, clause, corps, index in batterie():
        if filtre and filtre not in libelle:
            continue
        vues = [(nom, interroge(base, clause, corps, index))
                for nom, base in cibles]
        total += 1
        g, d = vues[0][1], vues[1][1]
        if comparable(g) == comparable(d) or meme_ordre_aux_ex_aequo(g, d):
            continue
        if assume(libelle, corps, g, d):
            assumes += 1
            classe = classe_assumee(libelle, corps)
            par_classe[classe] = par_classe.get(classe, 0) + 1
            print(f"~ {libelle}")
            print(f"      assume ({classe}) : {REFUS_ASSUMES[classe][1]}")
        else:
            ecarts += 1
            print(f"* {libelle}")
        for nom, vu in vues:
            print(f"      {nom}={abrege(vu)}")

    print(f"\n{total - ecarts - assumes}/{total} identiques, "
          f"{assumes} refus assumes, {ecarts} ecarts")
    if par_classe:
        print("  refus assumes par classe : "
              + ", ".join(f"{k}={v}" for k, v in sorted(par_classe.items())))
    return 1 if ecarts else 0


if __name__ == "__main__":
    sys.exit(main())
