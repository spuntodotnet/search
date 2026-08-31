#!/usr/bin/env python3
"""ferrite et Elasticsearch rendent-ils les memes documents sur un **motif** ?

`regexp`, `wildcard`, `prefix` et `match_phrase_prefix` ont ceci de particulier
que leur syntaxe est celle de **Lucene**, pas celle du moteur d'expressions
regulieres qui les execute chez ferrite. Les deux se ressemblent assez pour
qu'on croie pouvoir passer le motif tel quel, et divergent la ou personne ne
regarde : `\\d` a valu la lettre `d` jusqu'a Lucene 8 et « un chiffre » depuis,
`^` et `$` ne sont pas des ancres, `~` et `&` sont des operateurs, `@` veut dire
« n'importe quelle chaine » (ce qui pique dans un motif d'adresse e-mail), et
`case_insensitive` ne replie que l'ASCII — et seulement les caracteres isoles,
pas les plages.

Aucune de ces regles n'est deductible : elles se mesurent. Ce script pose les
memes motifs aux deux serveurs sur un corpus construit expres pour les visiter
(casse melangee, accents, chiffres, et tous les caracteres qui ont un sens
special quelque part), et compare les documents rendus.

    python3 tests/compat/diff_motifs.py [ferrite_url] [es_url]

Outil de developpement : exige un Elasticsearch 8.x lance a cote (Docker).
"""
import sys

from elasticsearch import ApiError, Elasticsearch

FERRITE = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:9200"
ES = sys.argv[2] if len(sys.argv) > 2 else "http://localhost:9201"
INDEX = "compat_motifs"

MAPPINGS = {
    "properties": {
        "ref": {"type": "keyword"},
        "libelle": {"type": "text"},
    }
}

# Des valeurs choisies une par une : chacune est la seule a distinguer deux
# lectures possibles d'un motif.
REFS = [
    "Audit-2026", "audit-2026", "AUDIT-2026", "audit_2026", "audits-2026.08.01",
    "audits-2026.07.31", "REF-0042", "ref-42", "ref-7", "x7y", "7", "42",
    "Edition", "edition", "Édition", "édition", "ecole", "école",
    "abc", "ABC", "aBc", "abcd", "ab", "aab", "adc", "aDc", "a7c", "a_c", "a c",
    "a-c", "a.c", "a*b", "a?b", "a+b", "a|b", "a(b", "a)b", "a[b", "a]b",
    "a{b", "a}b", "a~b", "a&b", "a^b", "a$b", "a#b", "a@b", "a<b", "a>b",
    'a"b', "a\\b", "user@example.com", "USER@EXAMPLE.COM", "2026-08-01",
]

LIBELLES = [
    "reduction de bruit ambiant",
    "reduction de bruit actif",
    "reduction du prix constatee",
    "batterie longue duree",
    "batteries longues durees",
    "audit de securite mensuel",
    "audit de securite annuel",
    "audit interne",
    "la presse parisienne du matin",
    "presse a chaud",
]


def documents():
    docs = []
    for i, ref in enumerate(REFS):
        docs.append((f"r{i}", {"ref": ref, "libelle": LIBELLES[i % len(LIBELLES)]}))
    for i, libelle in enumerate(LIBELLES):
        docs.append((f"l{i}", {"ref": f"libelle-{i}", "libelle": libelle}))
    return docs


def requetes():
    """(label, requete, ordonnee) — `ordonnee` quand le score classe vraiment.

    Les clauses par motif rendent un score constant : comparer l'ordre y
    comparerait l'ordre interne des documents, pas une decision du moteur. Seul
    `match_phrase_prefix` est classe par BM25, donc compare dans l'ordre.
    """
    q = []

    def re_(motif, **kw):
        v = {"value": motif, **kw}
        libelle = f"regexp [{motif}]" + ("".join(f" {k}={v}" for k, v in kw.items()))
        q.append((libelle, {"regexp": {"ref": v}}, False))

    # --- l'usage reel : contient / commence par / finit par
    re_("audit-2026")
    re_(".*audit.*")
    re_("audit.*")
    re_(".*2026")
    re_(".*AUDIT.*")
    re_("audit-2026", case_insensitive=True)
    re_(".*audit.*", case_insensitive=True)
    re_("audit.*", case_insensitive=True)
    re_(".*2026", case_insensitive=True)
    re_("audits-2026\\.0[78]\\..*")

    # --- classes, repetitions, alternatives
    re_("[a-z]+-[0-9]+")
    re_("[A-Z]+-[0-9]+")
    re_("[a-z]+-[0-9]+", case_insensitive=True)
    re_("ref-[0-9]{1,4}")
    re_("(edition|ecole)")
    re_("(Edition|ecole)", case_insensitive=True)
    re_("a.c")
    re_("a[bc]")
    re_("a[^bc]c")
    re_("a[^bc]c", case_insensitive=True)
    re_("a[d-e]c")
    re_("a[d-e]c", case_insensitive=True)
    re_("a[d]c", case_insensitive=True)
    re_("ab?c?d?")
    re_("(ab)+")
    re_("a{1,2}b.*")

    # --- le `|` sans branche gauche : ce n'est **pas** une alternation vide
    #
    # `parseConcatExp` de Lucene lit toujours au moins un atome, et
    # `parseSimpleExp` rend un caractere **litteral** devant tout ce qu'il ne
    # reconnait pas — `|` compris. Donc `|a` cherche la chaine `|a`, `a||b`
    # cherche `a` ou `|b`, et `a|` echoue (`unexpected end-of-string`) parce que
    # l'atome exige apres le `|` manque. ferrite en faisait des branches vides :
    # `|a` rendait les documents vides **et** `a`, en 200. Le corpus porte `a|b`
    # exprès, donc l'ecart se voit sur des documents et pas seulement sur un
    # statut. Trouve par une plage de graines neuves du fuzzer (8210220).
    re_("|a")
    re_("a||b")
    re_("|")
    re_("(|)")
    re_("()")
    re_("a|")
    re_("(a|)")
    re_("a|b|")
    re_("x*|y")

    # --- classes predefinies : ASCII chez Lucene, Unicode chez `regex`
    re_("\\d+")
    re_("a\\dc")
    re_("a\\Dc")
    re_("a\\wc")
    re_("a\\Wc")
    re_("a\\sc")
    re_("a\\Sc")
    re_("[\\d]+")
    re_("[\\w]+")
    re_("[^\\d]+")
    re_("\\w+@\\w+\\.com")

    # --- ce qui a un sens special quelque part, et pas au meme endroit
    re_("^abc$")            # `^` et `$` ne sont pas des ancres chez Lucene
    re_("a\\*b")
    re_("a\\\\b")
    re_('"a*b"')            # chaine litterale
    re_("user@.*")          # `@` = ANYSTRING : le piege du motif d'e-mail
    re_("a@")
    re_("a\\|b")
    re_("a\\(b")
    re_("a\\[b")
    re_("a\\{b")
    re_("a\\^b")
    re_("a\\$b")
    re_("a\\#b")
    re_("a\\<b")
    re_('a\\"b')
    re_("a~b", flags="NONE")
    re_("a&b", flags="NONE")
    re_("a#b", flags="NONE")
    re_("a<b", flags="NONE")
    re_("édition")
    re_("édition", case_insensitive=True)
    re_("[éÉ]dition")
    re_(".*é.*")

    # --- wildcard / prefix : les memes pieges, une syntaxe plus pauvre
    for motif in ("audit*", "*2026", "audit?2026", "a*b", "a\\*b", "AUDIT*", "*"):
        q.append((f"wildcard [{motif}]", {"wildcard": {"ref": motif}}, False))
    for motif in ("audit*", "AUDIT*", "*AUDIT*"):
        q.append((f"wildcard [{motif}] insensible",
                  {"wildcard": {"ref": {"value": motif, "case_insensitive": True}}}, False))
    for motif in ("audit", "Audit", "AUDIT", "éd", "a"):
        q.append((f"prefix [{motif}]", {"prefix": {"ref": motif}}, False))
        q.append((f"prefix [{motif}] insensible",
                  {"prefix": {"ref": {"value": motif, "case_insensitive": True}}}, False))

    # --- match_phrase_prefix : classe par le score, donc compare dans l'ordre
    for texte in ("reduction de bru", "reduction de bruit", "reduction", "red",
                  "audit de sec", "audit", "batterie lon", "batteri",
                  "la presse par", "presse", "durees", "duree", "zzz",
                  "reduction bru"):
        q.append((f"match_phrase_prefix [{texte}]",
                  {"match_phrase_prefix": {"libelle": texte}}, True))
    q.append(("match_phrase_prefix max_expansions=1",
              {"match_phrase_prefix": {"libelle": {"query": "batterie lon",
                                                   "max_expansions": 1}}}, True))
    q.append(("match_phrase_prefix dans un bool",
              {"bool": {"must": [{"match_phrase_prefix": {"libelle": "audit de sec"}}],
                        "filter": [{"prefix": {"ref": "libelle-"}}]}}, True))

    # --- les motifs que les deux doivent refuser
    for motif in ("Zol\\a", "a(b", "a[b-", "a{2,", "abc\\"):
        q.append((f"motif malforme [{motif}]", {"regexp": {"ref": motif}}, False))
    return q


class Comparateur:
    def __init__(self):
        self.ferrite = Elasticsearch(FERRITE, request_timeout=60)
        self.es = Elasticsearch(ES, request_timeout=60)
        self.total = 0
        self.identiques = 0
        self.refus_communs = 0
        self.ecarts = []

    def prepare(self, docs):
        for client, nom in ((self.ferrite, "ferrite"), (self.es, "ES")):
            client.options(ignore_status=404).indices.delete(index=INDEX)
            client.indices.create(index=INDEX, mappings=MAPPINGS,
                                  settings={"number_of_shards": 1,
                                            "number_of_replicas": 0})
            ops = []
            for doc_id, doc in docs:
                ops.append({"index": {"_index": INDEX, "_id": doc_id}})
                ops.append(doc)
            client.bulk(operations=ops)
            client.indices.refresh(index=INDEX)
            n = client.search(index=INDEX, query={"match_all": {}},
                              size=0)["hits"]["total"]["value"]
            print(f"   {nom:<8} {n} documents indexes")

    def _cherche(self, client, query):
        try:
            r = client.search(index=INDEX, query=query, size=100)
            return [h["_id"] for h in r["hits"]["hits"]], None
        except ApiError as exc:
            return None, exc.body["error"]["type"]

    def compare(self, label, query, ordonnee):
        self.total += 1
        idf, erreur_f = self._cherche(self.ferrite, query)
        ide, erreur_e = self._cherche(self.es, query)

        if erreur_e is not None:
            # ES refuse : ferrite doit refuser aussi, sinon il repond a une
            # question qu'Elasticsearch juge mal posee.
            if erreur_f is not None:
                self.refus_communs += 1
                self.identiques += 1
            else:
                self.ecarts.append((label, f"ES refuse ({erreur_e}), ferrite rend "
                                           f"{len(idf)} documents"))
            return
        if erreur_f is not None:
            self.ecarts.append((label, f"ferrite refuse ({erreur_f}), "
                                       f"ES rend {len(ide)} documents"))
            return

        if not ordonnee:
            idf, ide = sorted(idf), sorted(ide)
        if idf == ide:
            self.identiques += 1
            return
        if sorted(idf) == sorted(ide):
            self.ecarts.append((label, f"memes documents, ordre different : "
                                       f"{idf[:6]} vs {ide[:6]}"))
            return
        manque = sorted(set(ide) - set(idf))
        trop = sorted(set(idf) - set(ide))
        self.ecarts.append((label,
                            f"{len(idf)} vs {len(ide)} documents ; "
                            f"manquants={manque[:6]} en trop={trop[:6]}"))

    def run(self):
        print(f"== ferrite : {FERRITE}\n== ES      : {ES}\n")
        docs = documents()
        print(f"== indexation de {len(docs)} documents")
        self.prepare(docs)

        qs = requetes()
        print(f"\n== {len(qs)} motifs, poses aux deux serveurs\n")
        for label, query, ordonnee in qs:
            self.compare(label, query, ordonnee)

        for label, detail in self.ecarts:
            print(f"  [ecart] {label}\n          {detail}")
        print()
        print(f"  {self.identiques}/{self.total} motifs : memes documents "
              f"(dont {self.refus_communs} refuses des deux cotes)")
        return 1 if self.ecarts else 0


if __name__ == "__main__":
    sys.exit(Comparateur().run())
