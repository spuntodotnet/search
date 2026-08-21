#!/usr/bin/env python3
"""Les analyzers de ferrite decoupent-ils comme ceux d'Elasticsearch ?

Chaque analyzer integre est confronte a son homonyme d'ES sur un vocabulaire
large, via l'API `_analyze` des deux cotes. On compare la **suite ordonnee de
tokens, avec leurs positions et leurs offsets**.

Pourquoi les positions et les offsets, et pas seulement les termes : un
n-gramme mal positionne casse `match_phrase` **sans changer le compte de
tokens**, et un offset compte en octets la ou Java compte en unites UTF-16 ne
se voit sur aucun texte ASCII. Les deux se sont averes faux avant que cette
comparaison n'existe.

C'est la seule mesure qui compte : deux analyzers qui portent le meme nom mais
ne rendent pas les memes termes rendent aussi des resultats de recherche
differents, en silence. Le chiffre produit ici decide de ce qui est supporte et
de ce qui est refuse.

    python3 tests/compat/diff_analyzers.py [ferrite_url] [es_url]

Outil de developpement : exige un Elasticsearch 8.x lance a cote (Docker).
"""
import sys

from elasticsearch import ApiError, Elasticsearch

FERRITE = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:9200"
ES = sys.argv[2] if len(sys.argv) > 2 else "http://localhost:9201"

ANALYZERS = ["standard", "simple", "whitespace", "keyword", "stop", "english", "french"]

# Du francais courant, avec ce qui fait trebucher : elisions, accents, traits
# d'union, chiffres, pluriels irreguliers, majuscules.
TEXTES = [
    "Le Horla",
    "Bel-Ami",
    "l'ascension sociale d'un arriviste dans la presse parisienne",
    "la greve des mineurs du nord de la France",
    "un homme se croit habite par une presence invisible",
    "les chevaux courent dans le pre",
    "le cheval court dans le pre",
    "L'ÉDITION originale de 1885 coûte 250 euros",
    "qu'il s'agisse d'articles ou d'ouvrages",
    "jusqu'au bout de la nuit",
    "aujourd'hui c'est different",
    "des editions reliees et des editions brochees",
    "nous travaillons, ils travaillaient, elle travaillera",
    "the running dogs run quickly",
    "a beautiful house with beautiful gardens",
    "email: contact@exemple.fr, tel 01-23-45-67-89",
    "version 2.1.3 du logiciel",
    "C++ et C# ne sont pas Java",
    "Elasticsearch, tantivy & Lucene",
    "Ceci    a  des    espaces   multiples",
    "MAJUSCULES minuscules MiXtEs",
    "naive noel coeur soeur",
    "naïve noël cœur sœur",
    "un tres tres tres long mot: anticonstitutionnellement",
    "1885 1887 1880",
    "l'oeuvre d'une vie",
    "s'il te plait",
    "n'est-ce pas",
]

# Un stemmer ne se juge pas sur quelques phrases : ce vocabulaire couvre les
# familles de suffixes que les deux algorithmes traitent differemment. Chaque
# ligne est passee telle quelle a `_analyze` des deux cotes.
VOCABULAIRE_FR = """
chevaux journaux travaux bateaux eaux cheveux yeux nationaux
mineurs mineur mineure mineures editions edition editrice editeur
arriviste arrivistes journaliste journalistes socialisme socialiste
finissement finissant grandissement rougissant etablissement
lentement rapidement doucement gouvernement mouvement changement
directrice directeur creatrice createur acteur actrice
verificatrice verificateur simplificateur multiplicatrice
chanteuse chanteur vendeuse vendeur porteuse porteur
premiere derniere maniere lumiere carriere
troisieme quatrieme dixieme centieme
nationalite qualite quantite realite
heureux malheureux nombreux courageux
president presidence presidente presidents
parlant parlante parlants parlantes
maison maisons cheval chevaux animal animaux
naive naif naives naifs
belle beau belles beaux nouvelle nouveau
grande grandes grand grands petite petites petit petits
manger mangera mangerait mangeaient mange manges
finir finira finissait fini finie finis finies
""".split()

VOCABULAIRE_EN = """
running runner runs ran runningly
happiness happily happy happier
national nationalism nationalize nationality
relational relativity relative relatives
conditional conditionally condition conditions
argument arguments arguing argued argues
beautiful beautifully beauty beauties
organization organizational organize organizer organizing
sensitivity sensitiveness sensible sensibility
electricity electrical electric electrician
communism communist community communicate
adjustment adjustable adjusting adjusted
dependent dependence depending depends
probate rate cease controlling rolling
skies sky flies fly dying dies died
generalization feudalism callousness hopefulness
""".split()



# ---------------------------------------------------------------------------
# Les n-grammes
# ---------------------------------------------------------------------------
#
# Un `ngram` / `edge_ngram` se declare, il n'a pas de nom tout fait : chaque
# ligne ci-dessous est un corps de `_analyze` **complet**, pose aux deux
# serveurs. Les bornes sont celles qu'on trouve dans de vrais mappings (celles
# de Wagtail v7.1 en premier), plus les bords qui ne se devinent pas — un token
# plus court que `min_gram`, un `side: back`, un `token_chars` qui coupe.
#
# `max_ngram_diff` vaut 1 par defaut chez ES : les ecarts plus larges ne sont
# jouables que sur un index qui l'a releve, d'ou l'index dedie plus bas.

GRAMMES_LIBRES = [
    ("tokenizer ngram, defauts", {"tokenizer": "ngram"}),
    ("tokenizer edge_ngram, defauts", {"tokenizer": "edge_ngram"}),
    ("filtre ngram cite par son nom", {"tokenizer": "keyword", "filter": ["ngram"]}),
    ("filtre edge_ngram cite par son nom", {"tokenizer": "keyword", "filter": ["edge_ngram"]}),
    ("tokenizer ngram 2-3", {"tokenizer": {"type": "ngram", "min_gram": 2, "max_gram": 3}}),
    ("tokenizer ngram 2-3, lettres", {"tokenizer": {
        "type": "ngram", "min_gram": 2, "max_gram": 3, "token_chars": ["letter"]}}),
    ("tokenizer ngram 2-3, lettres+chiffres", {"tokenizer": {
        "type": "ngram", "min_gram": 2, "max_gram": 3, "token_chars": ["letter", "digit"]}}),
    ("tokenizer ngram 2-3, ponctuation", {"tokenizer": {
        "type": "ngram", "min_gram": 2, "max_gram": 3, "token_chars": ["punctuation"]}}),
    ("tokenizer ngram 2-3, symboles", {"tokenizer": {
        "type": "ngram", "min_gram": 2, "max_gram": 3, "token_chars": ["symbol"]}}),
    ("tokenizer ngram 2-3, espaces", {"tokenizer": {
        "type": "ngram", "min_gram": 2, "max_gram": 3, "token_chars": ["whitespace"]}}),
    ("tokenizer ngram 1-2, custom", {"tokenizer": {
        "type": "ngram", "min_gram": 1, "max_gram": 2,
        "token_chars": ["letter", "custom"], "custom_token_chars": "-'"}}),
    ("tokenizer edge_ngram 1-2, lettres", {"tokenizer": {
        "type": "edge_ngram", "min_gram": 1, "max_gram": 2, "token_chars": ["letter"]}}),
    ("filtre ngram 1-2 sur standard", {"tokenizer": "standard", "filter": [
        {"type": "ngram", "min_gram": 1, "max_gram": 2}]}),
    ("filtre ngram 3-4 sur standard", {"tokenizer": "standard", "filter": [
        {"type": "ngram", "min_gram": 3, "max_gram": 4}]}),
    ("filtre edge_ngram 1-4 sur standard", {"tokenizer": "standard", "filter": [
        {"type": "edge_ngram", "min_gram": 1, "max_gram": 4}]}),
    ("filtre edge_ngram 1-4 par la fin", {"tokenizer": "standard", "filter": [
        {"type": "edge_ngram", "min_gram": 1, "max_gram": 4, "side": "back"}]}),
    ("filtre edge_ngram 3-3, tokens courts jetes", {"tokenizer": "standard", "filter": [
        {"type": "edge_ngram", "min_gram": 3, "max_gram": 3}]}),
    ("filtre ngram 3-3 + preserve_original", {"tokenizer": "standard", "filter": [
        {"type": "ngram", "min_gram": 3, "max_gram": 3, "preserve_original": True}]}),
    ("filtre edge_ngram 2-2 back + preserve", {"tokenizer": "standard", "filter": [
        {"type": "edge_ngram", "min_gram": 2, "max_gram": 2,
         "side": "back", "preserve_original": True}]}),
    ("filtre edge_ngram sur keyword (titre entier)", {"tokenizer": "keyword", "filter": [
        {"type": "edge_ngram", "min_gram": 1, "max_gram": 6}]}),
    ("la chaine de Wagtail, bornes reduites", {"tokenizer": "standard", "filter": [
        "asciifolding", "lowercase", {"type": "edge_ngram", "min_gram": 1, "max_gram": 2}]}),
]

# Les memes, mais avec l'ecart large que Wagtail declare : ils exigent un index
# qui a releve `index.max_ngram_diff`, donc ils passent par ses analyzers.
INDEX_GRAMMES = "diff_analyzers_ngram"
REGLAGES_GRAMMES = {
    "settings": {
        "index": {"max_ngram_diff": 12},
        "analysis": {
            "tokenizer": {
                "ngram_tokenizer": {"type": "ngram", "min_gram": 3, "max_gram": 15},
                "edgengram_tokenizer": {"type": "edge_ngram", "min_gram": 2, "max_gram": 15,
                                        "side": "front"},
                "edge_mots": {"type": "edge_ngram", "min_gram": 1, "max_gram": 15,
                              "token_chars": ["letter", "digit"]},
            },
            "filter": {
                "ngram": {"type": "ngram", "min_gram": 3, "max_gram": 15},
                "edgengram": {"type": "edge_ngram", "min_gram": 1, "max_gram": 15},
            },
            "analyzer": {
                # Les deux analyzers de Wagtail v7.1, mot pour mot.
                "ngram_analyzer": {"type": "custom", "tokenizer": "standard",
                                   "filter": ["asciifolding", "lowercase", "ngram"]},
                "edgengram_analyzer": {"type": "custom", "tokenizer": "standard",
                                       "filter": ["asciifolding", "lowercase", "edgengram"]},
                "par_tokenizer": {"type": "custom", "tokenizer": "ngram_tokenizer"},
                "edge_par_tokenizer": {"type": "custom", "tokenizer": "edgengram_tokenizer"},
                "edge_mots_analyzer": {"type": "custom", "tokenizer": "edge_mots",
                                       "filter": ["lowercase"]},
            },
        },
    }
}
ANALYZERS_GRAMMES = list(REGLAGES_GRAMMES["settings"]["analysis"]["analyzer"])

# `token_chars` nomme des categories generales d'Unicode, lues chez Lucene par
# `Character.getType`. Les predicats de Rust n'y correspondent pas (`Ⅰ` est
# alphabetique sans etre une lettre, `½` est numerique sans etre un chiffre) :
# cet echantillon demande sa classe a **chaque** caractere, des deux cotes.
ECHANTILLON = (
    "".join(chr(c) for c in range(0x21, 0x7F))
    + "".join(chr(c) for c in range(0xA0, 0x180))
    + "".join(chr(c) for c in range(0x2000, 0x2070))
    + "".join(chr(c) for c in range(0x20A0, 0x20C0))
    + "".join(chr(c) for c in range(0x2100, 0x2200, 3))
    + "".join(chr(c) for c in range(0x2200, 0x22FF, 3))
    + "".join(chr(c) for c in range(0x3000, 0x3030))
    + "\t\n\x0b\x0c\r\x1c\x1d\x1e\x1f   "
    + "αΩあ漢한ЖΔﬁⅠⅡ①²³½٣۴०"
)
CLASSES = ["letter", "digit", "whitespace", "punctuation", "symbol"]


def tokens(client, texte, **corps):
    """La suite ordonnee (terme, offsets, position) — pas seulement les termes."""
    r = client.indices.analyze(text=texte, **corps)
    return [(t["token"], t["start_offset"], t["end_offset"], t["position"]) for t in r["tokens"]]


def compare(f, e, cas, textes, resume, titre):
    """Pose les memes questions aux deux serveurs, imprime les trois premiers ecarts."""
    for nom, corps in cas:
        identiques = 0
        exemples = []
        for texte in textes:
            try:
                a = tokens(f, texte, **corps)
            except ApiError as ex:
                a = f"REFUSE {ex.body.get('error', {}).get('type')} : " \
                    f"{ex.body.get('error', {}).get('reason')}"
            b = tokens(e, texte, **corps)
            if a == b:
                identiques += 1
            elif len(exemples) < 3:
                exemples.append((texte, a, b))
        total = len(textes)
        etat = "identique" if identiques == total else f"{identiques}/{total}"
        resume.append((f"{titre}{nom}", identiques, total))
        print(f"  [{etat:>9}] {nom}")
        for texte, a, b in exemples:
            print(f"              texte   : {texte!r}")
            print(f"              ferrite : {a}")
            print(f"              ES      : {b}")
        if exemples:
            print()


def classes_de(client, index=None):
    """Pour chaque classe de `token_chars`, l'ensemble des caracteres qu'elle retient."""
    out = {}
    for classe in CLASSES:
        r = client.indices.analyze(
            tokenizer={"type": "ngram", "min_gram": 1, "max_gram": 1, "token_chars": [classe]},
            text=ECHANTILLON,
        )
        out[classe] = {t["token"] for t in r["tokens"]}
    return out


def main():
    f = Elasticsearch(FERRITE, request_timeout=60)
    e = Elasticsearch(ES, request_timeout=60)
    # Une sonde differentielle qui ne trouve qu'un serveur annonce « tout
    # identique » : elle ne compare rien du tout.
    for nom, client in (("ferrite", f), ("ES", e)):
        if not client.ping():
            print(f"!! {nom} ne repond pas — une comparaison a un seul serveur ne mesure rien")
            return 2

    textes = TEXTES + VOCABULAIRE_FR + VOCABULAIRE_EN
    resume = []

    print(f"== {len(textes)} textes x {len(ANALYZERS)} analyzers integres\n")
    compare(f, e, [(a, {"analyzer": a}) for a in ANALYZERS], textes, resume, "")

    print(f"\n== {len(textes)} textes x {len(GRAMMES_LIBRES)} declarations de n-grammes\n")
    compare(f, e, GRAMMES_LIBRES, textes, resume, "ngram : ")

    print(f"\n== les analyzers de Wagtail v7.1, sur un index a max_ngram_diff = 12\n")
    for client in (f, e):
        client.options(ignore_status=404).indices.delete(index=INDEX_GRAMMES)
        client.indices.create(index=INDEX_GRAMMES, **REGLAGES_GRAMMES)
    compare(
        f, e,
        [(a, {"analyzer": a, "index": INDEX_GRAMMES}) for a in ANALYZERS_GRAMMES],
        textes, resume, "index : ",
    )
    for client in (f, e):
        client.options(ignore_status=404).indices.delete(index=INDEX_GRAMMES)

    print(f"\n== les classes de `token_chars`, sur {len(ECHANTILLON)} caracteres\n")
    ca, cb = classes_de(f), classes_de(e)
    for classe in CLASSES:
        manque = sorted(cb[classe] - ca[classe])
        en_trop = sorted(ca[classe] - cb[classe])
        etat = "identique" if not manque and not en_trop else f"{len(manque)}+{len(en_trop)} ecarts"
        resume.append((f"token_chars : {classe}", 0 if (manque or en_trop) else 1, 1))
        print(f"  [{etat:>9}] {classe} ({len(cb[classe])} caracteres chez ES)")
        if manque:
            print(f"              ES les classe, ferrite non : {[hex(ord(c)) for c in manque[:20]]}")
        if en_trop:
            print(f"              ferrite les classe, ES non : {[hex(ord(c)) for c in en_trop[:20]]}")

    print()
    imparfaits = [(a, i, t) for a, i, t in resume if i != t]
    print(f"  identiques a ES : {len(resume) - len(imparfaits)}/{len(resume)} batteries")
    for a, i, t in imparfaits:
        print(f"  divergent       : {a} ({t - i}/{t})")
    return 0 if not imparfaits else 1


if __name__ == "__main__":
    sys.exit(main())
