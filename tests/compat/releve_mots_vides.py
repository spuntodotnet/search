#!/usr/bin/env python3
"""Relever la liste de mots vides d'un analyzer d'Elasticsearch, mot a mot.

    python3 tests/compat/releve_mots_vides.py [es_url] [analyzer]

La liste de mots vides de l'analyzer `french` d'Elasticsearch n'est ni celle de
Snowball (elle garde `est`) ni l'ancienne de Lucene (elle retire `ceci`,
`cette`, `avec`, `sans`, `ils`). Plutot que de la deviner, on la **releve** :
chaque candidat est passe a `_analyze`, et un mot qui ne rend aucun token est un
mot vide.

La sortie est du Rust, a recopier dans `src/analysis.rs`. Ce script existe pour
que ce relevé soit **refaisable** : si la liste bouge d'une version d'ES a
l'autre, on la remesure au lieu de la reconstituer de memoire.

Limite, et elle est reelle : la liste obtenue est exacte **pour les candidats
proposes**. Un mot vide qui ne figurerait dans aucun des candidats ci-dessous
serait manque — c'est pourquoi ils couvrent large (articles, pronoms,
prepositions, conjonctions, adverbes frequents, et toute la conjugaison des
auxiliaires). `diff_analyzers.py` reste l'arbitre.
"""
import json
import sys
import urllib.request

ES = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:9201"
ANALYZER = sys.argv[2] if len(sys.argv) > 2 else "french"

CANDIDATS = """
a à afin ai aie aient aies ainsi ait alors annee annees après as assez au aucun
aucune aujourd auquel aura aurai auraient aurais aurait auras aurez aurions
auriez aurons auront aussi autant autre autres aux auxquelles auxquels avaient
avais avait avant avec avez aviez avions avoir avons ayant ayante ayantes
ayants aye ayez ayons beaucoup bien bon c ça car ce ceci cela celle celles
celui cependant certain certaine certaines certains ces cet cette ceux chacun
chaque chez ci combien comme comment concernant contre d dans de debout dedans
dehors delà depuis derrière des désormais desquelles desquels dessous dessus
deux devant devers devra doit donc dont du duquel durant dès elle elles en
encore entre envers environ es est et etaient etais etait etant etc ete etes
etions etre eu eue eues eurent eus eusse eussent eusses eussiez eussions eut
eux eûmes eût eûtes fait faire fais faisait fait fois font furent fus fusse
fussent fusses fussiez fussions fut fûmes fût fûtes hors ici il ils j jamais je
jusqu jusque l la laquelle le lequel les lesquelles lesquels leur leurs lors
lorsque lui m ma mais malgré me meme memes mes mien mienne miens moi moins mon
même mêmes n ne ni non nos notre nôtre nous nul on ont ou où oui par parce
parmi pas pendant peu peut peuvent plus plusieurs pour pourquoi près puis
puisque qu quand quant que quel quelle quelles quels qui quoi quoique s sa sans
sauf se sein selon sera serai seraient serais serait seras serez seriez serions
serons seront ses seulement si sien sienne siens soi soient sois soit sommes
son sont sous soyez soyons suis sur t ta te tes tienne tiens toi ton toujours
tous tout toute toutes très trop tu un une va vais vers voici voilà vont vos
votre vôtre vous vu y à ça étaient étais était étant étante étantes étants
étiez étions êtes été étée étées étés être
""".split()


def analyse(mots):
    corps = json.dumps({"analyzer": ANALYZER, "text": mots}).encode()
    req = urllib.request.Request(
        f"{ES}/_analyze", data=corps, method="POST",
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)["tokens"]


def main():
    candidats = sorted(set(CANDIDATS))
    # Une passe par mot : `_analyze` sur une liste rendrait des positions, pas
    # de quoi savoir *quel* mot a disparu.
    vides = []
    for i in range(0, len(candidats), 1):
        mot = candidats[i]
        if not analyse([mot]):
            vides.append(mot)
    print(f"// Relevé sur {ES} ({ANALYZER}) : {len(vides)} mots vides "
          f"sur {len(candidats)} candidats.", file=sys.stderr)
    largeur, ligne = 0, []
    for m in vides:
        ligne.append(f'"{m}"')
        largeur += len(m) + 4
        if largeur > 80:
            print("    " + ", ".join(ligne) + ",")
            largeur, ligne = 0, []
    if ligne:
        print("    " + ", ".join(ligne) + ",")


if __name__ == "__main__":
    main()
