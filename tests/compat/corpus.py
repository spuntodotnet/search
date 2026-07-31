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
