#!/usr/bin/env python3
"""Les analyzers de ferrite decoupent-ils comme ceux d'Elasticsearch ?

Chaque analyzer integre est confronte a son homonyme d'ES sur un vocabulaire
large, via l'API `_analyze` des deux cotes. On compare la **suite de tokens**,
mot a mot.

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

# Les analyzers de langue sont refuses par ferrite (le stemmer de tantivy n'est
# pas celui de Lucene) : voir docs/compat.md. La suite de compat verifie le refus.
ANALYZERS = ["standard", "simple", "whitespace", "keyword", "stop"]

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


def tokens(client, analyzer, texte):
    r = client.indices.analyze(analyzer=analyzer, text=texte)
    return [t["token"] for t in r["tokens"]]


def main():
    f = Elasticsearch(FERRITE, request_timeout=60)
    e = Elasticsearch(ES, request_timeout=60)

    print(f"== {len(TEXTES)} textes x {len(ANALYZERS)} analyzers\n")
    resume = []
    for analyzer in ANALYZERS:
        identiques = 0
        exemples = []
        for texte in TEXTES:
            try:
                a = tokens(f, analyzer, texte)
            except ApiError as ex:
                a = f"REFUSE {ex.body.get('error', {}).get('type')}"
            b = tokens(e, analyzer, texte)
            if a == b:
                identiques += 1
            elif len(exemples) < 3:
                exemples.append((texte, a, b))

        total = len(TEXTES)
        etat = "identique" if identiques == total else f"{identiques}/{total}"
        resume.append((analyzer, identiques, total))
        print(f"  [{etat:>9}] {analyzer}")
        for texte, a, b in exemples:
            print(f"              texte   : {texte}")
            print(f"              ferrite : {a}")
            print(f"              ES      : {b}")
        if exemples:
            print()

    print()
    parfaits = [a for a, i, t in resume if i == t]
    imparfaits = [(a, i, t) for a, i, t in resume if i != t]
    print(f"  identiques a ES : {', '.join(parfaits) if parfaits else 'aucun'}")
    for a, i, t in imparfaits:
        print(f"  divergent       : {a} ({t - i}/{t} textes)")
    return 0 if not imparfaits else 1


if __name__ == "__main__":
    sys.exit(main())
