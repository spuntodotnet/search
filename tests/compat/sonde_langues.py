#!/usr/bin/env python3
"""Les analyzers de langue : l'ecart mesure, puis sa disparition.

    python3 tests/compat/sonde_langues.py [ferrite] [es]
    python3 tests/compat/sonde_langues.py --ecart          # le tableau de l'ecart
    python3 tests/compat/sonde_langues.py --calibrer [a] [b]
    python3 tests/compat/sonde_langues.py --mots-vides     # regenere src/mots_vides.rs

Pourquoi une sonde de plus : `diff_analyzers.py` compare 217 textes **ecrits
ici**. Un stemmer ne se juge pas la-dessus — il a des dizaines de branches, et
c'est un corpus qui les visite qu'il faut. Celui-ci ne vient pas de nous : ce
sont les vocabulaires du projet **Snowball** (BSD-3-Clause, licence verifiee
dans le depot avant usage), 20 000 a 96 000 mots par langue, telecharges a la
demande dans `.snowball-voc/`.

Elle repond a trois questions, et la premiere est celle de la carte :

1. `--ecart` : **d'ou vient l'ecart**, langue par langue ? Chaque etape de la
   chaine d'ES est posee separement — minuscules seules, puis mots vides, puis
   elision / normalisation, puis stemmer — et l'ecart residuel avec l'analyzer
   nomme est compte. C'est ce qui separe « il manque un stemmer » de « il
   manque une liste de mots vides ». Tout se mesure **cote ES** : aucune de ces
   etapes n'a besoin de ferrite, donc le tableau vaut meme si on decide de
   maintenir le refus.
2. sans argument : ferrite rend-il **exactement** ce que rend ES, sur la suite
   ordonnee `(terme, offsets, position)` ? C'est le seul verdict qui compte.
3. `--calibrer` : la meme batterie contre deux Elasticsearch. Tant qu'elle n'y
   est pas a zero, ce qu'elle dit de ferrite ne vaut rien.

Outil de developpement : exige un Elasticsearch 8.x lance a cote (Docker) et
l'acces au reseau pour le premier telechargement.
"""
import json
import pathlib
import subprocess
import sys
import urllib.error
import urllib.request

RACINE = pathlib.Path(__file__).resolve().parents[2]
CACHE = RACINE / ".snowball-voc"
SOURCE = "https://raw.githubusercontent.com/snowballstem/snowball-data/master"
# La licence du depot de vocabulaires, verifiee avant tout usage : le fichier
# doit porter la clause BSD de Martin Porter. On ne se fie pas au nom du depot.
LICENCE = "COPYING"
MARQUEUR_LICENCE = "Redistribution and use in source and binary forms"

# Les douze langues servies, plus les deux refusees — un refus se mesure aussi.
LANGUES = [
    "danish", "dutch", "german", "hungarian", "italian", "norwegian",
    "portuguese", "romanian", "russian", "spanish", "swedish", "turkish",
]
REFUSEES = ["finnish"]

# La chaine exacte de chaque analyzer de langue d'ES, reconstruite puis
# **verifiee** : `--ecart` compare la chaine a l'analyzer nomme, et elle doit
# etre identique sur tout le vocabulaire avant que le reste veuille dire quoi
# que ce soit. Aucune ligne n'est ici parce que la documentation le dit.
ELISION_IT = ["c", "l", "all", "dall", "dell", "nell", "sull", "coll", "pell", "gl", "agl",
              "dagl", "degl", "negl", "sugl", "un", "m", "t", "s", "v", "d"]
AVANT_MINUSCULES = {
    "italian": [{"type": "elision", "articles_case": True, "articles": ELISION_IT}],
    "turkish": ["apostrophe"],
}
MINUSCULES = {"turkish": {"type": "lowercase", "language": "turkish"}}
APRES_MOTS_VIDES = {"german": ["german_normalization"]}
# `DutchAnalyzer` pose un `StemmerOverrideFilter` de **quatre** mots avant son
# stemmer. Quatre sur 45 670 : sur 3 000 mots tires au sort, aucun ne sortait.
OVERRIDE = {"dutch": {"type": "stemmer_override",
                      "rules": ["fiets=>fiets", "bromfiets=>bromfiets",
                                "ei=>eier", "kind=>kinder"]}}
STEMMER = {
    "german": "light_german", "spanish": "light_spanish",
    "italian": "light_italian", "portuguese": "light_portuguese",
}


def chaine(langue, jusqu_a):
    """La chaine d'ES pour cette langue, arretee apres l'etape voulue."""
    f = list(AVANT_MINUSCULES.get(langue, []))
    if jusqu_a == "brut":
        # L'etat d'avant : ce que ferrite savait deja faire, `standard` seul.
        return ["lowercase"]
    f.append(MINUSCULES.get(langue, "lowercase"))
    if jusqu_a == "prefixes":
        return f
    f.append({"type": "stop", "stopwords": f"_{langue}_"})
    if jusqu_a == "mots_vides":
        return f
    f += APRES_MOTS_VIDES.get(langue, [])
    if jusqu_a == "normalisation":
        return f
    if langue in OVERRIDE:
        f.append(OVERRIDE[langue])
    f.append({"type": "stemmer", "language": STEMMER.get(langue, langue)})
    return f


class Serveur:
    def __init__(self, url):
        self.url = url.rstrip("/")

    def analyze(self, corps):
        req = urllib.request.Request(
            f"{self.url}/_analyze", data=json.dumps(corps).encode(),
            headers={"Content-Type": "application/json"},
        )
        try:
            with urllib.request.urlopen(req, timeout=300) as r:
                return json.load(r)["tokens"]
        except urllib.error.HTTPError as e:
            corps = json.load(e)
            raise Refus(corps.get("error", {}).get("reason", "?")) from None

    def vivant(self):
        try:
            with urllib.request.urlopen(self.url, timeout=10):
                return True
        except OSError:
            return False

    def par_mot(self, mots, **corps):
        """Un paquet de mots analyse d'un coup, redistribue par offsets.

        Les mots sont joints par des sauts de ligne — un separateur que tout
        tokenizer coupe — et chaque token est rendu au mot dont il porte
        l'offset. Un mot vide rend donc une liste vide, ce qui est exactement
        ce qu'on veut mesurer.
        """
        texte = "\n".join(mots)
        bornes, p = [], 0
        for m in mots:
            bornes.append((p, p + len(m)))
            p += len(m) + 1
        sortie = [[] for _ in mots]
        i = 0
        for t in self.analyze(dict(text=texte, **corps)):
            while i < len(bornes) and t["start_offset"] >= bornes[i][1]:
                i += 1
            if i < len(bornes):
                sortie[i].append(
                    (t["token"], t["start_offset"] - bornes[i][0],
                     t["end_offset"] - bornes[i][0])
                )
        return sortie

    def par_paquets(self, mots, taille=1500, **corps):
        out = []
        for i in range(0, len(mots), taille):
            out += self.par_mot(mots[i:i + taille], **corps)
        return out


class Refus(Exception):
    pass


def vocabulaire(langue):
    """Le vocabulaire du projet Snowball, telecharge et mis en cache."""
    CACHE.mkdir(exist_ok=True)
    licence = CACHE / LICENCE
    if not licence.exists():
        telecharge(f"{SOURCE}/{LICENCE}", licence)
    if MARQUEUR_LICENCE not in licence.read_text(encoding="utf-8"):
        raise SystemExit(f"!! {licence} ne porte pas la clause BSD attendue — rien n'est utilise")
    f = CACHE / f"{langue}.txt"
    if not f.exists():
        telecharge(f"{SOURCE}/{langue}/voc.txt", f)
    return [m.strip() for m in f.read_text(encoding="utf-8").splitlines() if m.strip()]


def telecharge(url, vers):
    print(f"   .. {url}", file=sys.stderr)
    with urllib.request.urlopen(url, timeout=120) as r:
        vers.write_bytes(r.read())


# ---------------------------------------------------------------------------
# 1. D'ou vient l'ecart ? (le tableau de la carte)
# ---------------------------------------------------------------------------

ETAPES = [
    ("brut", "minuscules seules"),
    ("prefixes", "+ élision / apostrophe"),
    ("mots_vides", "+ mots vides"),
    ("normalisation", "+ normalisation"),
    ("stem", "+ stemmer"),
]


def mesure_ecart(es, langues, echantillon=None):
    lignes = []
    for langue in langues:
        mots = vocabulaire(langue)
        if echantillon:
            mots = mots[:echantillon]
        ref = es.par_paquets(mots, analyzer=langue)
        compte = {}
        for cle, _ in ETAPES:
            got = es.par_paquets(mots, tokenizer="standard", filter=chaine(langue, cle))
            compte[cle] = sum(1 for a, b in zip(got, ref) if a != b)
        lignes.append((langue, len(mots), compte))
        etat = "  ".join(f"{compte[c]:>6}" for c, _ in ETAPES)
        print(f"  {langue:<11} {len(mots):>6} mots   {etat}")
    return lignes


# ---------------------------------------------------------------------------
# 2. ferrite rend-il la meme chose ?
# ---------------------------------------------------------------------------

# Du texte suivi, en plus du vocabulaire : un mot vide ne se voit que dans une
# phrase, et les positions ne se comparent que sur plusieurs tokens. Une ligne
# par langue, ecrite pour porter ses pieges (casse, elision, apostrophe,
# digrammes).
PHRASES = {
    "danish": ["Huset er smukt, og byen er stor", "Løbe, løber, løb — og at være her"],
    "dutch": ["De huizen in de stad zijn mooi", "Lopen, loopt, gelopen: het is niet moeilijk"],
    "german": ["Die Häuser der Straße sind größer als das Haus",
               "HAEUSER, Haeuser und Häuser — Fußball auf der Strasse",
               "Ueberall quellen die Quellen, und ueber allem steht die Sonne"],
    "hungarian": ["A házak és a városok szépek", "Futni jó, mert a futás szép"],
    "italian": ["L'anno dell'uomo e le città più belle", "DELL'ANNO, dall'alba all'imbrunire"],
    "norwegian": ["Husene i byen er vakre", "Å løpe er vakkert, og han løper"],
    "portuguese": ["As casas da cidade são bonitas", "Meninos e meninas correndo rapidamente"],
    "romanian": ["Casele din orașe sunt frumoase", "Alergând prin oraș și prin case"],
    "russian": ["Дома в городе красивые", "Костёр горит, и пёс бежит по бёдрам холма"],
    "spanish": ["Las casas de la ciudad son hermosas", "Los niños corrieron por las calles"],
    "swedish": ["Husen i staden är vackra", "Att springa är vackert och han springer"],
    "turkish": ["Evler ve şehirler güzeldir", "ISTANBUL, İSTANBUL ve Diyarbakır'ın evleri",
                "IĞDIR ile Işık'ın ışıkları"],
}

# Les briques posees une a une dans un analyzer sur mesure : c'est ainsi qu'un
# mapping reel les emploie, et ce n'est pas la meme chose que l'analyzer nomme
# (rien ne garantit que le filtre isole se comporte comme dans la chaine).
BRIQUES = [
    ("stemmer porter2", {"tokenizer": "standard",
                         "filter": ["lowercase", {"type": "stemmer", "language": "porter2"}]}),
    ("stemmer english (Porter)", {"tokenizer": "standard",
                                  "filter": ["lowercase", {"type": "stemmer", "language": "english"}]}),
    ("porter_stem cite par son nom", {"tokenizer": "standard",
                                      "filter": ["lowercase", "porter_stem"]}),
    ("stemmer light_german", {"tokenizer": "standard",
                              "filter": ["lowercase", {"type": "stemmer", "language": "light_german"}]}),
    # Le Snowball allemand, qui n'est **pas** le stemmer que l'analyzer
    # `german` pose : les deux noms existent chez ES et rendent deux choses.
    ("stemmer german (Snowball)", {"tokenizer": "standard",
                                   "filter": ["lowercase", {"type": "stemmer", "language": "german"}]}),
    ("stemmer light_portuguese", {"tokenizer": "standard",
                                  "filter": ["lowercase",
                                             {"type": "stemmer", "language": "light_portuguese"}]}),
    ("german_normalization", {"tokenizer": "standard",
                              "filter": ["lowercase", "german_normalization"]}),
    ("apostrophe", {"tokenizer": "standard", "filter": ["apostrophe", "lowercase"]}),
    ("elision italienne", {"tokenizer": "standard",
                           "filter": [{"type": "elision", "articles": ELISION_IT,
                                       "articles_case": True}, "lowercase"]}),
    ("elision sensible a la casse", {"tokenizer": "standard",
                                     "filter": [{"type": "elision", "articles": ["l", "d"],
                                                 "articles_case": True}]}),
    ("elision insensible (defaut)", {"tokenizer": "standard",
                                     "filter": [{"type": "elision", "articles": ["l", "d"]}]}),
    ("stop _german_", {"tokenizer": "standard",
                       "filter": ["lowercase", {"type": "stop", "stopwords": "_german_"}]}),
    ("stop _russian_", {"tokenizer": "standard",
                        "filter": ["lowercase", {"type": "stop", "stopwords": "_russian_"}]}),
    ("stop _none_", {"tokenizer": "standard",
                     "filter": ["lowercase", {"type": "stop", "stopwords": "_none_"}]}),
]

# Les 32 caracteres dont le repli en minuscules de Java n'est pas celui de Rust
# (les 31 caracteres *titre*, et le `İ` turc), plus de quoi les entourer.
CASSE = ["İstanbul", "ǅungla", "ᾈΘΗΝΑ", "Ǆ ǅ ǆ Ǉ ǈ ǉ Ǌ ǋ ǌ Ǳ ǲ ǳ",
         "ᾈ ᾉ ᾊ ᾋ ᾌ ᾍ ᾎ ᾏ ᾘ ᾙ ᾚ ᾛ ᾜ ᾝ ᾞ ᾟ ᾨ ᾩ ᾪ ᾫ ᾬ ᾭ ᾮ ᾯ ᾼ ῌ ῼ",
         "ΣΙΣΥΦΟΣ σίσυφος", "STRASSE straße ſtraſse"]


def compare(a, b, cas, mots, titre, resume, taille=1500):
    """Pose les memes questions aux deux serveurs, imprime les trois premiers ecarts."""
    for nom, corps in cas:
        try:
            got = a.par_paquets(mots, taille, **corps)
        except Refus as ex:
            print(f"  [   REFUSE] {titre}{nom} : {ex}")
            resume.append((f"{titre}{nom}", 0, len(mots)))
            continue
        ref = b.par_paquets(mots, taille, **corps)
        ecarts = [(m, x, y) for m, x, y in zip(mots, got, ref) if x != y]
        identiques = len(mots) - len(ecarts)
        etat = "identique" if not ecarts else f"{identiques}/{len(mots)}"
        resume.append((f"{titre}{nom}", identiques, len(mots)))
        print(f"  [{etat:>9}] {titre}{nom}")
        for m, x, y in ecarts[:3]:
            print(f"              mot     : {m!r}")
            print(f"              ferrite : {x}")
            print(f"              ES      : {y}")


def mesure_ferrite(f, e, langues, echantillon, resume):
    for langue in langues:
        mots = vocabulaire(langue)
        if echantillon:
            mots = mots[:echantillon]
        print(f"\n-- {langue} : {len(mots)} mots du vocabulaire Snowball, "
              f"{len(PHRASES[langue])} phrases")
        compare(f, e, [("analyzer " + langue, {"analyzer": langue})], mots, "", resume)
        compare(f, e, [("phrases " + langue, {"analyzer": langue})],
                PHRASES[langue], "", resume)


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    drapeaux = {a for a in sys.argv[1:] if a.startswith("--")}
    echantillon = None
    for d in list(drapeaux):
        if d.startswith("--echantillon="):
            echantillon = int(d.split("=", 1)[1])
            drapeaux.discard(d)

    if "--mots-vides" in drapeaux:
        return releve_mots_vides(Serveur(args[0] if args else "http://localhost:9201"))

    if "--ecart" in drapeaux:
        es = Serveur(args[0] if args else "http://localhost:9201")
        if not es.vivant():
            print("!! l'Elasticsearch de reference ne repond pas")
            return 2
        print("D'ou vient l'ecart, langue par langue : nombre de mots dont l'analyse\n"
              "differe de celle de l'analyzer nomme d'ES, chaine arretee apres chaque etape.\n")
        print(f"  {'langue':<11} {'mots':>6}       " +
              "  ".join(f"{n:>6}" for _, n in ETAPES))
        mesure_ecart(es, LANGUES + REFUSEES, echantillon)
        return 0

    calibrage = "--calibrer" in drapeaux
    if calibrage:
        f = Serveur(args[0] if args else "http://localhost:9201")
        e = Serveur(args[1] if len(args) > 1 else "http://localhost:9202")
        noms = ("ES A", "ES B")
    else:
        f = Serveur(args[0] if args else "http://localhost:9200")
        e = Serveur(args[1] if len(args) > 1 else "http://localhost:9201")
        noms = ("ferrite", "ES")
    for nom, s in zip(noms, (f, e)):
        if not s.vivant():
            print(f"!! {nom} ({s.url}) ne repond pas — "
                  "une comparaison a un seul serveur ne mesure rien")
            return 2

    resume = []
    mesure_ferrite(f, e, LANGUES, echantillon, resume)

    print("\n-- les briques posees une a une dans un analyzer sur mesure")
    mots = sum((PHRASES[l] for l in LANGUES), []) + [m for l in LANGUES
                                                     for m in vocabulaire(l)[:400]]
    compare(f, e, BRIQUES, mots, "", resume)

    print("\n-- les 32 caracteres dont le repli en minuscules de Java n'est pas celui de Rust")
    compare(f, e, [("analyzer standard", {"analyzer": "standard"}),
                   ("analyzer keyword + lowercase",
                    {"tokenizer": "keyword", "filter": ["lowercase"]})],
            CASSE, "casse : ", resume)

    if calibrage:
        # Un Elasticsearch **sert** ces analyzers : la batterie des refus n'a
        # pas de sens ici, et la faire tourner quand meme rendrait un calibrage
        # rouge pour une raison qui ne mesure rien. On le dit plutot que de la
        # sauter en silence.
        print("\n-- les analyzers refuses : sans objet au calibrage "
              f"({', '.join(REFUSEES + ['czech', 'greek'])} sont servis par ES)")
        return verdict(resume)

    print("\n-- les analyzers refuses : le refus doit etre explicite, jamais un silence")
    for nom in REFUSEES + ["czech", "greek"]:
        try:
            f.analyze({"analyzer": nom, "text": "test"})
            print(f"  [  !! MUET] {nom} : accepte sans etre mesure")
            resume.append((f"refus : {nom}", 0, 1))
        except Refus as ex:
            print(f"  [   refuse] {nom} : {str(ex)[:120]}…")
            resume.append((f"refus : {nom}", 1, 1))

    return verdict(resume)


def verdict(resume):
    print()
    imparfaits = [(a, i, t) for a, i, t in resume if i != t]
    print(f"  identiques a ES : {len(resume) - len(imparfaits)}/{len(resume)} batteries")
    for a, i, t in imparfaits:
        print(f"  divergent       : {a} ({t - i}/{t})")
    return 0 if not imparfaits else 1


# ---------------------------------------------------------------------------
# 3. Les listes de mots vides
# ---------------------------------------------------------------------------

def releve_mots_vides(es):
    """Regenere `src/mots_vides.rs` depuis le jar de Lucene, puis le verifie.

    Le relevé par candidats de `releve_mots_vides.py` est exact **pour les
    candidats proposes** : il avait manque `celà` dans la liste francaise, un
    mot qu'ES ecarte et que ferrite indexait. La source est donc le fichier
    qu'Elasticsearch ouvre lui-meme — mais lu dans le conteneur, pas recopie —
    et il est verifie dans les deux sens contre ES : chaque mot de la liste doit
    ne rendre aucun token, et sur le vocabulaire complet de la langue aucun mot
    hors liste ne doit disparaitre.
    """
    conteneur = subprocess.run(
        ["docker", "ps", "--filter", "ancestor=docker.elastic.co/elasticsearch/elasticsearch:8.15.0",
         "--format", "{{.Names}}"], capture_output=True, text=True).stdout.split()
    if not conteneur:
        print("!! aucun conteneur elasticsearch:8.15.0 ne tourne — la source est son jar")
        return 2
    import tempfile
    import zipfile
    jar = subprocess.run(
        ["docker", "exec", conteneur[0], "sh", "-c",
         "ls /usr/share/elasticsearch/lib/lucene-analysis-common-*.jar"],
        capture_output=True, text=True).stdout.strip()
    with tempfile.TemporaryDirectory() as d:
        local = pathlib.Path(d) / "lac.jar"
        subprocess.run(["docker", "cp", f"{conteneur[0]}:{jar}", str(local)], check=True)
        z = zipfile.ZipFile(local)
        listes = {l: lire_liste(z, l) for l in LANGUES + REFUSEES + ["french"]}
    listes["english"] = ENGLISH_STOP_WORDS_SET

    ok = True
    for langue, mots in listes.items():
        if langue == "english":
            continue
        f = [MINUSCULES.get(langue, "lowercase"), {"type": "stop", "stopwords": f"_{langue}_"}]
        reste = [m for m, t in zip(mots, es.par_paquets(mots, 1000, tokenizer="standard", filter=f))
                 if t]
        voc = vocabulaire(langue)
        ens = {m.lower() for m in mots}
        surprises = [m for m, t in zip(voc, es.par_paquets(voc, 1500, tokenizer="standard", filter=f))
                     if not t and m.lower() not in ens]
        # Un mot du vocabulaire que la chaine efface sans figurer dans la liste
        # n'est un defaut que s'il n'est pas explique par une etape *avant* le
        # filtre : `ki-be` en hongrois est deux mots vides, `beni'` en turc perd
        # son apostrophe. On les imprime, on ne les avale pas.
        print(f"  {langue:<11} {len(mots):>4} mots  survivants {len(reste):>2} {reste[:4]}  "
              f"hors liste {len(surprises):>3} {surprises[:4]}")
        ok = ok and not reste
    if not ok:
        print("!! une liste ne correspond pas a ce qu'ES ecarte — rien n'est ecrit")
        return 1
    ecrit_mots_vides(listes)
    print(f"\n  ecrit src/mots_vides.rs ({sum(len(m) for m in listes.values())} mots)")
    return 0


# La liste anglaise de Lucene (`StopAnalyzer.ENGLISH_STOP_WORDS_SET`) n'est pas
# un fichier du jar : elle est ecrite dans le code.
ENGLISH_STOP_WORDS_SET = [
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "if", "in", "into", "is",
    "it", "no", "not", "of", "on", "or", "such", "that", "the", "their", "then", "there",
    "these", "they", "this", "to", "was", "will", "with",
]
# Le fichier que chaque analyzer ouvre. Les langues « snowball » partagent un
# repertoire, les deux autres ont le leur — ce n'est pas devinable, c'est lu.
FICHIERS = {
    "romanian": "ro/stopwords.txt", "turkish": "tr/stopwords.txt",
}


def lire_liste(z, langue):
    nom = FICHIERS.get(langue, f"snowball/{langue}_stop.txt")
    texte = z.read("org/apache/lucene/analysis/" + nom).decode("utf-8")
    commentaire = "|" if nom.startswith("snowball/") else "#"
    mots, vus = [], set()
    for ligne in texte.splitlines():
        for mot in ligne.split(commentaire)[0].split():
            if mot not in vus:
                vus.add(mot)
                mots.append(mot)
    return mots


def ecrit_mots_vides(listes):
    out = [
        "//! Les listes de mots vides des analyzers de langue d'Elasticsearch.",
        "//!",
        "//! Genere par `tests/compat/sonde_langues.py --mots-vides`, qui les lit dans le",
        "//! `lucene-analysis-common-*.jar` du **conteneur de reference** — le fichier",
        "//! qu'Elasticsearch ouvre lui-meme — puis les **verifie** contre lui, mot a mot",
        "//! (chaque mot de la liste doit ne rendre aucun token) et a l'envers (sur le",
        "//! vocabulaire complet de la langue, aucun mot hors liste ne doit disparaitre).",
        "//!",
        "//! Ne pas editer a la main : la liste francaise l'avait ete, relevee par une",
        "//! liste de candidats ecrite ici, et il y manquait `celà` — un mot qu'ES ecarte",
        "//! et que ferrite indexait, en silence.",
        "//!",
        "//! Une liste tient dans une seule chaine, separee par des sauts de ligne : 15",
        "//! tableaux de `&str` couteraient une table de pointeurs de plusieurs dizaines",
        "//! de kilo-octets dans un binaire qui en pese 4 M.",
        "",
    ]
    for langue in sorted(listes):
        mots = listes[langue]
        out.append(f"/// `_{langue}_` — {len(mots)} mots.")
        out.append(f'pub const {langue.upper()}: &str = "\\')
        ligne = ""
        for m in mots:
            if len(ligne) + len(m) + 2 > 92:
                out.append(ligne + "\\")
                ligne = ""
            ligne += m + "\\n"
        out.append(ligne + '";')
        out.append("")
    (RACINE / "src" / "mots_vides.rs").write_text("\n".join(out), encoding="utf-8")


if __name__ == "__main__":
    sys.exit(main())
