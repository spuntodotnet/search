#!/usr/bin/env python3
"""ferrite et Elasticsearch surlignent-ils **les memes passages** ?

    python3 tests/compat/diff_highlight.py [ferrite] [es]
    python3 tests/compat/diff_highlight.py --calibrer [es_a] [es_b]

Un fragment de surlignage n'est pas « le texte autour du mot trouve » : c'est
ce que le `UnifiedHighlighter` de Lucene decoupe, et rien de sa forme n'etait
devinable. Ce fichier pose donc la meme question aux deux serveurs et compare
la liste de fragments **caractere par caractere** — pas leur nombre, pas leur
presence : leur contenu exact, balises comprises.

Ce qu'il separe, et pourquoi chacune de ces lignes existe :

- **ou le fragment commence et finit.** Les phrases sont fusionnees vers
  l'avant tant que la longueur reste sous `fragment_size` ; une phrase qui
  deborde a elle seule est re-coupee **au mot**. Le corpus porte donc des
  textes construits pour les deux regimes, et fait varier `fragment_size`
  autour du seuil : c'est la que les deux lectures divergent ;
- **ou une phrase s'arrete.** Un point suivi d'une **minuscule** n'en termine
  pas une (UAX#29, regle SB8), un point entre deux capitales non plus. Trois
  textes du corpus ne sont la que pour ca ;
- **quels fragments survivent** a `number_of_fragments` : ceux que le
  `PassageScorer` de Lucene note le mieux, puis remis dans l'ordre du
  document. Un texte a sept passages le mesure a chaque valeur de 1 a 6 ;
- **ce qui est marque** : `match`, `match_phrase` (une seule marque pour toute
  la suite), `prefix`, `wildcard`, `regexp`, `fuzzy`, `terms`, `range` sur un
  `keyword` — et ce qui ne marque **rien** (`match_all`, `exists`, `ids`, un
  `must_not`, un champ numerique) ;
- **`require_field_match`**, dans ses deux sens ;
- **un champ multivalue** : un fragment ne franchit jamais la frontiere entre
  deux valeurs, mais les fragments de toutes les valeurs sont en concurrence.

Comme toutes les sondes de ce depot, elle **refuse de tourner** si elle ne
trouve pas ses deux cibles : une comparaison qui ne compare rien annoncerait
« tout identique ».

`--calibrer` fait tourner la meme batterie contre **deux** Elasticsearch : tant
qu'elle n'y est pas a zero, ce qu'elle dit de ferrite ne vaut rien.

Outil de developpement : exige un Elasticsearch 8.15 lance a cote (Docker).
"""
import json
import re
import sys
import urllib.error
import urllib.request

INDEX = "diff-highlight"

MAPPINGS = {
    "properties": {
        "titre": {"type": "text"},
        "corps": {"type": "text"},
        "tag": {"type": "keyword"},
        "n": {"type": "integer"},
        "lignes": {"type": "nested", "properties": {"ref": {"type": "keyword"}}},
    }
}

# Six phrases, dont deux courtes : de quoi voir la fusion s'arreter a chaque
# valeur de `fragment_size`.
PROSE = (
    "Le chat dort sur le tapis. Le chien aboie dans le jardin voisin depuis ce matin. "
    "Un oiseau chante sur la branche du grand chene. Le chat se reveille et regarde "
    "l'oiseau avec attention. La nuit tombe doucement sur le village endormi. "
    "Le chat retourne dormir sur le tapis chaud pres de la cheminee."
)
# Une seule phrase au sens d'UAX#29 : les points sont suivis de minuscules.
UNE_SEULE = "zzz cible. aaa. bbb cible cible."
# Aucun terminateur du tout : tout se joue sur la re-coupe au mot.
SANS_POINT = "aaaaaaaaaa bbbb cible cccc dddddddd eeee cible ffffffff gggg"
# Sept passages de meme forme : c'est le classement des fragments qui decide.
SEPT = " ".join(f"Phrase {i} avec cible dedans." for i in range(7))
# Abreviation, capitale, nombre pointe : les regles SB6, SB7 et SB8.
ABREVIATIONS = "Dr. Martin est arrive. il est parti. La version 8.15 cible U.S.A. Fin."
# Accents et elision : les offsets ne sont pas des octets.
ACCENTS = (
    "Élan du matin. Le café était trés chaud, mais l'élan a bu quand meme. "
    "L'ascension sociale d'un arriviste cible le sommet. Fin."
)

DOCS = [
    ("prose", {"titre": "Le chat noir", "corps": PROSE, "tag": "animaux", "n": 1,
               "lignes": [{"ref": "alpha"}, {"ref": "beta"}]}),
    ("court", {"titre": "Court", "corps": "Un chat.", "tag": "bref", "n": 2}),
    ("rien", {"titre": "Sans rien", "corps": "Aucun animal ne passe ici.",
              "tag": "vide", "n": 3}),
    ("multi", {"titre": "Multi", "tag": "multi", "n": 4,
               "corps": ["Premier chat ici.", "Deuxieme passage sans rien.",
                         "Troisieme chat la et cible aussi."]}),
    ("multi2", {"titre": "Multi sans point", "tag": "multi", "n": 5,
                "corps": ["Premier chat ici", "Deuxieme sans rien", "Troisieme chat la"]}),
    ("une-seule", {"titre": "Une seule phrase", "corps": UNE_SEULE, "tag": "zzz", "n": 6}),
    ("sans-point", {"titre": "Sans point", "corps": SANS_POINT, "tag": "aaa", "n": 7}),
    ("sept", {"titre": "Sept", "corps": SEPT, "tag": "sept", "n": 8}),
    ("abrev", {"titre": "Abreviations", "corps": ABREVIATIONS, "tag": "abc", "n": 9}),
    ("accents", {"titre": "Accents", "corps": ACCENTS, "tag": "éàü", "n": 10}),
    ("regulier", {"titre": "Regulier", "tag": "reg", "n": 12,
                  "corps": ("un deux trois quatre cinq six sept huit neuf dix "
                            "mot long tres precis suivi de encore plus de mots "
                            "pour laisser de la place a droite du fragment")}),
    ("long", {"titre": "Mot long", "tag": "long", "n": 11,
              "corps": "cible " + "z" * 300 + " apres"}),
]


def cas():
    """Chaque cas : un nom, et le corps de `_search` a poser aux deux."""
    out = []

    def ajoute(nom, query, highlight):
        out.append((nom, {"query": query, "size": 20, "sort": ["n"],
                          "highlight": highlight}))

    chat = {"match": {"corps": "chat"}}
    cible = {"match": {"corps": "cible"}}

    # --- ou le fragment commence et finit ---------------------------------
    for taille in (0, 1, 2, 3, 5, 8, 10, 15, 19, 20, 21, 25, 30, 40, 60, 80,
                   100, 150, 300):
        ajoute(f"fragment_size={taille}", cible,
               {"fragment_size": taille, "fields": {"corps": {}}})
        ajoute(f"fragment_size={taille} (chat)", chat,
               {"fragment_size": taille, "fields": {"corps": {}}})
    ajoute("fragment_size negatif", cible,
           {"fragment_size": -1, "fields": {"corps": {}}})

    # --- combien de fragments, et lesquels --------------------------------
    for nb in (0, 1, 2, 3, 4, 5, 6, 10):
        ajoute(f"number_of_fragments={nb}", cible,
               {"number_of_fragments": nb, "fields": {"corps": {}}})
        ajoute(f"number_of_fragments={nb} fs=15", cible,
               {"number_of_fragments": nb, "fragment_size": 15,
                "fields": {"corps": {}}})
        ajoute(f"number_of_fragments={nb} fs=30 (chat)", chat,
               {"number_of_fragments": nb, "fragment_size": 30,
                "fields": {"corps": {}}})

    # --- la longueur de la correspondance decale le fragment --------------
    # Le fragment se centre sur le **milieu** de ce qui est marque : sur un mot
    # isole ca ne se voit pas, sur une phrase de quatre mots ca decale le bord
    # gauche de plusieurs mots. C'est le balayage qui l'a trouve.
    for mots in ("mot", "mot long", "mot long tres", "mot long tres precis"):
        for taille in (1, 5, 9, 13, 17, 21, 25, 33):
            q = ({"match_phrase": {"corps": mots}} if " " in mots
                 else {"term": {"corps": mots}})
            ajoute(f"phrase [{mots}] fs={taille}", q,
                   {"fragment_size": taille, "number_of_fragments": 1,
                    "fields": {"corps": {}}})

    # --- les balises ------------------------------------------------------
    ajoute("pre/post_tags", chat,
           {"pre_tags": ["<mark>"], "post_tags": ["</mark>"], "fields": {"corps": {}}})
    ajoute("pre/post_tags multiples", chat,
           {"pre_tags": ["<1>", "<2>"], "post_tags": ["</1>", "</2>"],
            "fields": {"corps": {}}})
    ajoute("tags_schema styled", chat, {"tags_schema": "styled", "fields": {"corps": {}}})
    ajoute("balises vides", chat, {"pre_tags": [], "post_tags": [], "fields": {"corps": {}}})
    ajoute("pre_tags seul", chat, {"pre_tags": ["<b>"], "fields": {"corps": {}}})
    ajoute("surcharge par champ", chat,
           {"pre_tags": ["<g>"], "post_tags": ["</g>"], "fragment_size": 200,
            "fields": {"corps": {"pre_tags": ["<c>"], "post_tags": ["</c>"],
                                 "fragment_size": 30}}})

    # --- no_match_size ----------------------------------------------------
    for n in (0, 1, 3, 10, 30, 40, 1000):
        ajoute(f"no_match_size={n}", {"term": {"tag": "animaux"}},
               {"no_match_size": n, "fields": {"corps": {}, "titre": {}}})
    ajoute("no_match_size sur champ absent", chat,
           {"no_match_size": 20, "fields": {"absent": {}}})

    # --- quels champs -----------------------------------------------------
    ajoute("motif *", chat, {"fields": {"*": {}}})
    ajoute("motif t*", chat, {"fields": {"t*": {}}})
    ajoute("champ numerique", {"term": {"n": 1}}, {"fields": {"n": {}}})
    ajoute("champ absent", chat, {"fields": {"absent": {}}})
    ajoute("fields vide", chat, {"fields": {}})
    ajoute("fields en liste", chat, {"fields": [{"corps": {}}, {"titre": {}}]})
    ajoute("deux champs", {"bool": {"should": [chat, {"match": {"titre": "chat"}}]}},
           {"fields": {"corps": {}, "titre": {}}})
    ajoute("sous-champ nested",
           {"nested": {"path": "lignes", "query": {"term": {"lignes.ref": "alpha"}}}},
           {"fields": {"lignes.ref": {}}})
    ajoute("nested sous motif",
           {"nested": {"path": "lignes", "query": {"term": {"lignes.ref": "beta"}}}},
           {"fields": {"*": {}}})

    # --- require_field_match ---------------------------------------------
    ajoute("require_field_match defaut", {"match": {"titre": "chat"}},
           {"fields": {"corps": {}}})
    ajoute("require_field_match false", {"match": {"titre": "chat"}},
           {"require_field_match": False, "fields": {"corps": {}}})
    ajoute("require_field_match false, motif", {"match": {"titre": "chat"}},
           {"require_field_match": False, "fields": {"*": {}}})

    # --- ce que chaque clause marque -------------------------------------
    clauses = [
        ("match un mot", chat),
        ("match deux mots", {"match": {"corps": "chat tapis"}}),
        ("match operator and", {"match": {"corps": {"query": "chat tapis",
                                                    "operator": "and"}}}),
        ("match_phrase", {"match_phrase": {"corps": "le chat"}}),
        ("match_phrase trois mots", {"match_phrase": {"corps": "sur le tapis"}}),
        ("match_phrase_prefix", {"match_phrase_prefix": {"corps": "le cha"}}),
        ("multi_match", {"multi_match": {"query": "chat", "fields": ["corps", "titre"]}}),
        ("multi_match phrase", {"multi_match": {"query": "le chat", "type": "phrase",
                                                "fields": ["corps", "titre"]}}),
        ("term keyword", {"term": {"tag": "animaux"}}),
        ("terms keyword", {"terms": {"tag": ["abc", "zzz", "animaux"]}}),
        ("range keyword", {"range": {"tag": {"gte": "a", "lt": "c"}}}),
        ("range numerique", {"range": {"n": {"gte": 1}}}),
        ("prefix", {"prefix": {"corps": "cha"}}),
        ("wildcard", {"wildcard": {"corps": "ch*n"}}),
        ("wildcard ?", {"wildcard": {"corps": "cha?"}}),
        ("regexp", {"regexp": {"corps": "ch.+e"}}),
        ("regexp classe", {"regexp": {"corps": "[cd]hat"}}),
        ("fuzzy", {"fuzzy": {"corps": "chien"}}),
        ("fuzzy distance 1", {"fuzzy": {"corps": {"value": "chot", "fuzziness": 1}}}),
        ("exists", {"exists": {"field": "corps"}}),
        ("match_all", {"match_all": {}}),
        ("ids", {"ids": {"values": ["prose"]}}),
        ("bool should", {"bool": {"should": [chat, {"term": {"corps": "tapis"}}]}}),
        ("bool filter", {"bool": {"filter": [chat]}}),
        ("bool must_not", {"bool": {"must": [chat],
                                    "must_not": [{"term": {"corps": "tapis"}}]}}),
        ("constant_score", {"constant_score": {"filter": chat}}),
        ("dis_max", {"dis_max": {"queries": [chat, {"match": {"titre": "chat"}}]}}),
        ("term avec boost", {"term": {"corps": {"value": "chat", "boost": 3}}}),
        ("prefix insensible", {"prefix": {"corps": {"value": "CHA",
                                                    "case_insensitive": True}}}),
    ]
    for nom, q in clauses:
        ajoute(f"clause {nom}", q, {"fields": {"corps": {}, "tag": {}}})
        ajoute(f"clause {nom} fs=25", q, {"fragment_size": 25,
                                          "fields": {"corps": {}, "tag": {}}})

    # --- ce qui doit etre refuse (des deux cotes, ou d'un seul) -----------
    for cle, valeur in [("type", "fvh"), ("type", "plain"), ("type", "unified"),
                        ("order", "score"), ("boundary_scanner", "word"),
                        ("encoder", "html"), ("fragmenter", "simple"),
                        ("phrase_limit", 10), ("max_analyzed_offset", 100),
                        ("nawak", 1)]:
        ajoute(f"refus {cle}={valeur}", chat, {cle: valeur, "fields": {"corps": {}}})
    ajoute("refus highlight_query", {"match_all": {}},
           {"highlight_query": chat, "fields": {"corps": {}}})
    ajoute("refus matched_fields", chat, {"fields": {"corps": {"matched_fields": ["corps"]}}})
    ajoute("order none", chat, {"order": "none", "fields": {"corps": {}}})

    return out


# ---------------------------------------------------------------------------


def appel(base, methode, chemin, corps=None):
    donnees = json.dumps(corps).encode() if corps is not None else None
    r = urllib.request.Request(base + chemin, data=donnees, method=methode,
                               headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(r, timeout=60) as f:
            return json.loads(f.read() or b"{}"), 200
    except urllib.error.HTTPError as e:
        return json.loads(e.read() or b"{}"), e.code
    except urllib.error.URLError:
        return None, 0


def vivant(base):
    corps, code = appel(base, "GET", "/")
    return code == 200 and corps is not None


def prepare(base):
    appel(base, "DELETE", "/" + INDEX)
    corps, code = appel(base, "PUT", "/" + INDEX, {
        "mappings": MAPPINGS,
        "settings": {"number_of_shards": 1, "number_of_replicas": 0},
    })
    if code != 200:
        raise SystemExit(f"{base} : creation de l'index refusee — {json.dumps(corps)}")
    for doc_id, doc in DOCS:
        appel(base, "PUT", f"/{INDEX}/_doc/{doc_id}?refresh=true", doc)
    appel(base, "POST", f"/{INDEX}/_refresh")


def surlignages(base, corps_requete):
    """La reponse reduite a ce qui se compare : les fragments, par document."""
    corps, code = appel(base, "POST", f"/{INDEX}/_search", corps_requete)
    if code != 200:
        err = (corps or {}).get("error", {})
        # ES prefixe ses erreurs de parsing par la position dans le corps
        # (`[1:82] `) ; ferrite ne tient pas de position de lecture. La
        # divergence est declaree dans docs/compat.md, pas lissee ailleurs.
        raison = re.sub(r"^\[\d+:\d+\] ", "", err.get("reason", ""))
        return {"__erreur__": err.get("type", "?"), "__raison__": raison}
    return {h["_id"]: h.get("highlight") for h in corps["hits"]["hits"]}


def refus_de(cote):
    return "__erreur__" in cote


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    calibrer = "--calibrer" in sys.argv
    gauche = args[0] if args else ("http://localhost:9201" if calibrer
                                   else "http://localhost:9200")
    droite = args[1] if len(args) > 1 else ("http://localhost:9202" if calibrer
                                            else "http://localhost:9201")
    nom_gauche = "ES-a" if calibrer else "ferrite"

    for base, nom in ((gauche, nom_gauche), (droite, "ES")):
        if not vivant(base):
            raise SystemExit(
                f"{nom} ne repond pas sur {base} — une sonde differentielle qui ne "
                f"trouve qu'un serveur annoncerait « tout identique ». Arret.")

    print(f"== {nom_gauche} {gauche}  vs  ES {droite}")
    prepare(gauche)
    prepare(droite)
    print(f"== {len(DOCS)} documents, {len(cas())} questions\n")

    identiques = refus = deux_refus = ecarts = 0
    details = []
    for nom, requete in cas():
        a = surlignages(gauche, requete)
        b = surlignages(droite, requete)
        if a == b:
            identiques += 1
            continue
        # Un refus explicite de ferrite la ou ES repond est un cout de
        # perimetre, pas un ecart : il se compte a part, et il se lit.
        if refus_de(a) and not refus_de(b):
            refus += 1
            details.append(("refus", nom, a["__raison__"][:140]))
            continue
        # Les deux refusent : la question n'est pas servie non plus chez ES.
        # Seul le texte du message differe, et personne ne le teste.
        if refus_de(a) and refus_de(b):
            deux_refus += 1
            details.append(("deux", nom, f"{a['__erreur__']} / ES {b['__erreur__']}"))
            continue
        ecarts += 1
        details.append(("ecart", nom,
                        _diff(a, b, nom_gauche)))

    for genre, nom, texte in details:
        etiquette = {"refus": "refus assume", "deux": "refuse des deux cotes"}.get(
            genre, "ECART")
        print(f"  [{etiquette}] {nom}\n      {texte}")
    if details:
        print()

    total = identiques + refus + deux_refus + ecarts
    print(f"  {identiques}/{total} questions : memes fragments, au caractere pres")
    print(f"  {deux_refus}/{total} refuses des deux cotes")
    print(f"  {refus}/{total} refus assumes ({nom_gauche} refuse la ou ES repond)")
    print(f"  {ecarts}/{total} ecarts reels")
    for base in (gauche, droite):
        appel(base, "DELETE", "/" + INDEX)
    return 1 if ecarts else 0


def _diff(a, b, nom_gauche):
    if refus_de(a) or refus_de(b):
        return (f"{nom_gauche}={json.dumps(a, ensure_ascii=False)[:200]}\n      "
                f"ES     ={json.dumps(b, ensure_ascii=False)[:200]}")
    lignes = []
    for cle in sorted(set(a) | set(b)):
        if a.get(cle) != b.get(cle):
            lignes.append(f"[{cle}] {nom_gauche}="
                          f"{json.dumps(a.get(cle), ensure_ascii=False)}")
            lignes.append(f"      [{cle}] ES     ="
                          f"{json.dumps(b.get(cle), ensure_ascii=False)}")
    return "\n      ".join(lignes[:8])


if __name__ == "__main__":
    sys.exit(main())
