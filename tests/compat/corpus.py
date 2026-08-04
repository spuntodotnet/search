"""Un corpus deterministe, assez gros et assez varie pour que la pertinence
veuille dire quelque chose.

Genere avec une graine fixe : deux executions produisent exactement les memes
documents, donc un ecart constate entre ferrite et Elasticsearch est
reproductible et attribuable au moteur, jamais au jeu de donnees.

Le vocabulaire suit une distribution deliberement desequilibree (quelques mots
tres frequents, une longue traine de mots rares) : c'est ce que BM25 exploite,
et donc ce qu'il faut pour comparer un classement.
"""
import random

SEED = 20260731
NB_DOCS = 600

MAPPINGS = {
    "properties": {
        "titre": {"type": "text"},
        "corps": {"type": "text"},
        "marque": {"type": "keyword"},
        "categorie": {"type": "keyword"},
        "tags": {"type": "keyword"},
        "prix": {"type": "double"},
        "stock": {"type": "integer"},
        "actif": {"type": "boolean"},
        "cree_le": {"type": "date"},
        "note": {"type": "double"},
    }
}

# Fréquent -> rare. La position dans la liste sert de poids.
VOCAB = [
    "appareil", "modele", "version", "usage", "qualite", "format", "systeme",
    "capacite", "vitesse", "puissance", "autonomie", "connexion", "ecran",
    "batterie", "capteur", "moteur", "boitier", "clavier", "cable", "support",
    "silencieux", "portable", "compact", "robuste", "leger", "resistant",
    "etanche", "reglable", "pliable", "modulaire", "sombre", "lumineux",
    "bluetooth", "filaire", "magnetique", "thermique", "optique", "acoustique",
    "numerique", "analogique", "hybride", "sans", "avec", "pour", "tres",
    "reduction", "bruit", "fond", "actif", "passif", "ambiant", "isolation",
    "restitution", "grave", "aigu", "medium", "spatial", "immersif",
    "chargement", "rapide", "induction", "secteur", "solaire", "amovible",
    "garantie", "livraison", "assemblage", "entretien", "nettoyage",
    "aluminium", "acier", "plastique", "bois", "verre", "tissu", "cuir",
]

MARQUES = ["Sony", "JBL", "Logitech", "Dell", "Bose", "Anker", "Samsung",
           "Philips", "Corsair", "Sennheiser", "Asus", "Belkin"]
CATEGORIES = ["audio", "peripherique", "ecran", "energie", "mobilier", "reseau"]
TAGS = ["promo", "nouveaute", "reconditionne", "pro", "eco", "bestseller"]


def _mot(rng):
    """Tire un mot avec un biais fort vers le debut de la liste."""
    i = min(int(abs(rng.gauss(0, len(VOCAB) / 3))), len(VOCAB) - 1)
    return VOCAB[i]


def documents():
    """Rend `[(id, document)]`, identique a chaque appel."""
    rng = random.Random(SEED)
    docs = []
    for n in range(NB_DOCS):
        titre = " ".join(_mot(rng) for _ in range(rng.randint(2, 5)))
        corps = " ".join(_mot(rng) for _ in range(rng.randint(15, 60)))
        doc = {
            "titre": titre,
            "corps": corps,
            "marque": rng.choice(MARQUES),
            "categorie": rng.choice(CATEGORIES),
            "prix": round(rng.uniform(5, 900), 2),
            "stock": rng.randint(0, 300),
            "actif": rng.random() < 0.75,
            "cree_le": "%04d-%02d-%02d" % (rng.randint(2023, 2026),
                                           rng.randint(1, 12), rng.randint(1, 28)),
        }
        # Champs parfois absents : de quoi exercer `exists` pour de vrai.
        if rng.random() < 0.6:
            doc["note"] = round(rng.uniform(1, 5), 1)
        if rng.random() < 0.5:
            doc["tags"] = rng.sample(TAGS, rng.randint(1, 3))
        docs.append((str(n + 1), doc))
    return docs


def bigrammes(docs, combien, rng):
    """Des suites de mots réellement présentes dans le corpus.

    Une requête de phrase tirée au hasard ne matcherait jamais rien : pour
    comparer `match_phrase`, il faut des phrases qui existent.
    """
    sources = [d["corps"].split() for _, d in docs if len(d["corps"].split()) > 3]
    out = []
    for _ in range(combien):
        mots = rng.choice(sources)
        i = rng.randrange(len(mots) - 2)
        out.append(" ".join(mots[i:i + rng.choice([2, 2, 3])]))
    return out


def requetes(docs):
    """La batterie de requetes qui exerce le corpus, deterministe elle aussi.

    Elle vit ici, avec le corpus qu'elle interroge, parce que trois outils s'en
    servent : la comparaison de pertinence contre un ES 8 (`diff_relevance.py`),
    celle contre un ES 7 (`diff_es7.py`), et le banc (`bench_vs_es.py`) — ce
    dernier sans client Elasticsearch du tout.
    """
    rng = random.Random(SEED + 1)
    mots = VOCAB
    phrases = bigrammes(docs, 14, rng)
    q = []

    # --- match : un, deux, trois termes, et l'operateur `and`
    for mot in mots[:14]:
        q.append((f"match corps [{mot}]", {"match": {"corps": mot}}, None))
    for _ in range(10):
        deux = f"{rng.choice(mots)} {rng.choice(mots)}"
        q.append((f"match corps [{deux}]", {"match": {"corps": deux}}, None))
        q.append((f"match and corps [{deux}]",
                  {"match": {"corps": {"query": deux, "operator": "and"}}}, None))
    for _ in range(6):
        trois = " ".join(rng.choice(mots) for _ in range(3))
        q.append((f"match corps [{trois}]", {"match": {"corps": trois}}, None))
    q.append(("match titre + corps identiques",
              {"bool": {"should": [{"match": {"titre": "ecran"}},
                                   {"match": {"corps": "ecran"}}]}}, None))

    # --- multi_match : la clause d'une barre de recherche
    for _ in range(8):
        mot = rng.choice(mots)
        q.append((f"multi_match best_fields [{mot}]",
                  {"multi_match": {"query": mot, "fields": ["titre", "corps"]}}, None))
        q.append((f"multi_match most_fields [{mot}]",
                  {"multi_match": {"query": mot, "fields": ["titre", "corps"],
                                   "type": "most_fields"}}, None))
        q.append((f"multi_match titre^3 [{mot}]",
                  {"multi_match": {"query": mot, "fields": ["titre^3", "corps"]}}, None))
    for _ in range(4):
        deux = f"{rng.choice(mots)} {rng.choice(mots)}"
        q.append((f"multi_match and [{deux}]",
                  {"multi_match": {"query": deux, "fields": ["titre", "corps"],
                                   "operator": "and"}}, None))
        q.append((f"multi_match tie_breaker [{deux}]",
                  {"multi_match": {"query": deux, "fields": ["titre", "corps"],
                                   "tie_breaker": 0.3}}, None))

    # --- multi_match : la recherche libre telle qu'une application l'ecrit
    # Une barre « chercher par reference / nom / montant » balaie des champs de
    # types differents : sans `lenient`, taper un mot fait echouer la recherche
    # entiere parce qu'un des champs vises est numerique.
    mixtes = ["titre", "corps", "marque", "prix", "stock", "cree_le"]
    for mot in mots[:6]:
        q.append((f"multi_match lenient champs mixtes [{mot}]",
                  {"multi_match": {"query": mot, "fields": mixtes, "lenient": True}}, None))
    q.append(("multi_match lenient, aucun champ ne sait lire la valeur",
              {"multi_match": {"query": "appareil", "fields": ["prix", "stock", "cree_le"],
                               "lenient": True}}, None))
    q.append(("multi_match lenient sur une valeur lisible partout",
              {"multi_match": {"query": "42", "fields": ["titre", "stock"],
                               "lenient": True}}, None))
    q.append(("multi_match lenient sous must_not (clause vide, rien d'exclu)", {"bool": {
        "must": [{"match": {"corps": "appareil"}}],
        "must_not": [{"multi_match": {"query": "appareil", "fields": ["prix"],
                                      "lenient": True}}]}}, None))
    # Un champ que le mapping ne connait pas est ecarte de la liste, pas fatal a
    # la clause : sinon la barre de recherche rend 0 document en silence.
    q.append(("multi_match champ non mappe + champ mappe",
              {"multi_match": {"query": "appareil", "fields": ["titre", "jamais_mappe"]}}, None))
    q.append(("multi_match champ non mappe seul",
              {"multi_match": {"query": "appareil", "fields": ["jamais_mappe"]}}, None))

    # `type: phrase` — la meme phrase dans chaque champ, puis dis_max.
    for phrase in phrases[:6]:
        q.append((f"multi_match phrase [{phrase}]",
                  {"multi_match": {"query": phrase, "fields": ["titre", "corps"],
                                   "type": "phrase"}}, None))
        q.append((f"multi_match phrase tie_breaker [{phrase}]",
                  {"multi_match": {"query": phrase, "fields": ["titre", "corps"],
                                   "type": "phrase", "tie_breaker": 0.3}}, None))
        q.append((f"multi_match phrase titre^2 [{phrase}]",
                  {"multi_match": {"query": phrase, "fields": ["titre^2", "corps"],
                                   "type": "phrase"}}, None))
    q.append(("multi_match phrase sur un keyword",
              {"multi_match": {"query": "Sony", "fields": ["marque", "categorie"],
                               "type": "phrase"}}, [{"prix": "asc"}]))
    q.append(("multi_match phrase lenient champs mixtes",
              {"multi_match": {"query": phrases[0], "fields": mixtes,
                               "type": "phrase", "lenient": True}}, None))

    # `type: phrase_prefix` — la meme barre pendant la frappe.
    for phrase in phrases[:6]:
        tronquee = phrase[:-2]
        q.append((f"multi_match phrase_prefix [{tronquee}]",
                  {"multi_match": {"query": tronquee, "fields": ["titre", "corps"],
                                   "type": "phrase_prefix"}}, None))
    q.append(("multi_match phrase_prefix max_expansions",
              {"multi_match": {"query": "reduction de bru", "fields": ["titre", "corps"],
                               "type": "phrase_prefix", "max_expansions": 3}}, None))
    q.append(("multi_match phrase_prefix lenient sur un keyword",
              {"multi_match": {"query": "reduct", "fields": ["corps", "marque"],
                               "type": "phrase_prefix", "lenient": True}}, None))

    # --- match_phrase : sur des suites qui existent vraiment
    for phrase in phrases:
        q.append((f"match_phrase [{phrase}]", {"match_phrase": {"corps": phrase}}, None))
    # `slop` est refuse par ferrite (voir docs/compat.md) : le comparer n'aurait
    # pas de sens, la suite de compat verifie le refus.
    q.append(("match_phrase mot unique", {"match_phrase": {"corps": "ecran"}}, None))

    # --- match_phrase_prefix : la barre de recherche pendant la frappe
    for phrase in phrases[:8]:
        # Le dernier mot ampute : c'est le cas d'usage, et c'est celui qui
        # classe (le score d'un prefixe developpe n'est pas constant).
        tronquee = phrase[:-2]
        q.append((f"match_phrase_prefix [{tronquee}]",
                  {"match_phrase_prefix": {"corps": tronquee}}, None))
    for mot in mots[:6]:
        q.append((f"match_phrase_prefix un terme [{mot[:4]}]",
                  {"match_phrase_prefix": {"corps": mot[:4]}}, None))
    q.append(("match_phrase_prefix max_expansions",
              {"match_phrase_prefix": {"corps": {"query": "reduction de bru",
                                                 "max_expansions": 3}}}, None))

    # --- motifs et identifiants
    for mot in mots[:6]:
        q.append((f"prefix corps [{mot[:4]}]", {"prefix": {"marque": mot[:2]}},
                  [{"prix": "asc"}]))
    for marque in MARQUES[:4]:
        q.append((f"wildcard [{marque}]", {"wildcard": {"marque": f"{marque[:2]}*"}},
                  [{"prix": "asc"}]))
        q.append((f"fuzzy [{marque}]", {"fuzzy": {"marque": marque[:-1] + "x"}},
                  [{"prix": "asc"}]))
        # `regexp` : les filtres « contient / commence par / finit par » d'un
        # service, insensibles a la casse comme on les ecrit en vrai.
        q.append((f"regexp contient [{marque}]",
                  {"regexp": {"marque": f".*{marque[1:3]}.*"}}, [{"prix": "asc"}]))
        q.append((f"regexp insensible [{marque}]",
                  {"regexp": {"marque": {"value": f".*{marque[1:3].upper()}.*",
                                         "case_insensitive": True}}}, [{"prix": "asc"}]))
    for motif in ("[A-Z][a-z]+", "S(ony|amsung)", ".*e[lr].*", "[A-Za-z]{4,6}",
                  "\\w+", "A.*|B.*"):
        q.append((f"regexp [{motif}]", {"regexp": {"marque": motif}}, [{"prix": "asc"}]))
    q.append(("regexp categorie insensible",
              {"regexp": {"categorie": {"value": "AUDI.", "case_insensitive": True}}},
              [{"prix": "asc"}]))
    q.append(("ids", {"ids": {"values": ["1", "5", "9", "42"]}}, [{"prix": "asc"}]))
    q.append(("constant_score", {"constant_score": {
        "filter": {"term": {"categorie": "audio"}}, "boost": 3.0}}, None))
    for _ in range(4):
        mot = rng.choice(mots)
        q.append((f"dis_max [{mot}]", {"dis_max": {"queries": [
            {"match": {"titre": mot}}, {"match": {"corps": mot}}]}}, None))
        q.append((f"dis_max tie [{mot}]", {"dis_max": {"queries": [
            {"match": {"titre": mot}}, {"match": {"corps": mot}}],
            "tie_breaker": 0.4}}, None))

    # --- exists
    for champ in ("note", "tags", "corps", "marque"):
        q.append((f"exists [{champ}]", {"exists": {"field": champ}},
                  [{"prix": "asc"}]))

    # --- term / terms / range, en contexte filtre (ordre par tri)
    for marque in MARQUES[:5]:
        q.append((f"term marque [{marque}]", {"term": {"marque": marque}},
                  [{"prix": "asc"}]))
    q.append(("terms marque x3",
              {"terms": {"marque": MARQUES[:3]}}, [{"prix": "desc"}]))
    q.append(("term actif=true", {"term": {"actif": True}}, [{"stock": "desc"}]))
    for lo, hi in ((0, 50), (50, 200), (200, 900)):
        q.append((f"range prix [{lo},{hi}[", {"range": {"prix": {"gte": lo, "lt": hi}}},
                  [{"prix": "asc"}]))
    q.append(("range stock > 150", {"range": {"stock": {"gt": 150}}}, [{"stock": "asc"}]))
    q.append(("range date >= 2025", {"range": {"cree_le": {"gte": "2025-01-01"}}},
              [{"cree_le": "asc"}]))

    # --- bool : les combinaisons qui font une vraie recherche a facettes
    for cat in CATEGORIES[:4]:
        q.append((f"bool must+filter [{cat}]", {"bool": {
            "must": [{"match": {"corps": rng.choice(mots)}}],
            "filter": [{"term": {"categorie": cat}},
                       {"range": {"prix": {"lt": 500}}}]}}, None))
    q.append(("bool must_not", {"bool": {
        "must_not": [{"term": {"marque": "Sony"}}]}}, [{"prix": "asc"}]))
    q.append(("bool should min=2", {"bool": {
        "should": [{"term": {"categorie": "audio"}},
                   {"term": {"actif": True}},
                   {"range": {"prix": {"lt": 100}}}],
        "minimum_should_match": 2}}, [{"prix": "asc"}]))
    q.append(("bool multi_match + filtres", {"bool": {
        "must": [{"multi_match": {"query": "bluetooth reduction",
                                  "fields": ["titre^2", "corps"]}}],
        "filter": [{"term": {"actif": True}}, {"exists": {"field": "note"}}]}}, None))

    # --- tris : plusieurs cles, plusieurs types
    q.append(("tri multi-cles categorie+prix", {"match_all": {}},
              [{"categorie": "asc"}, {"prix": "desc"}]))
    q.append(("tri note desc (valeurs manquantes)", {"match_all": {}},
              [{"note": "desc"}]))
    q.append(("tri marque asc", {"match_all": {}}, [{"marque": "asc"}]))
    return q
