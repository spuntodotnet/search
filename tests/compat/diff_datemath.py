#!/usr/bin/env python3
"""ferrite et Elasticsearch resolvent-ils une **borne de date** pareil ?

Une borne de date d'une requete n'est pas une date. C'est une expression que le
serveur resout, et qu'il arrondit **selon le cote de la borne** :

- `{"lt": "now"}` — le filtre « en retard » de n'importe quel KPI — se resout
  cote serveur ; le lire comme une date litterale rend un 400 la ou ES rend des
  documents ;
- `{"lte": "2026-03-15"}` couvre la journee entiere chez ES, `{"lt": ...}` la
  meme chaine s'arrete a minuit. Un moteur qui prend les deux pour le meme
  instant rend **moins de documents** qu'ES sans que rien ne le signale ;
- `{"term": {"d": "2026-03-15"}}` n'est pas une egalite : c'est la journee.

Rien de tout ca n'est deductible d'une lecture : ES arrondit l'ancre d'un
`2026-03-16||-1d` vers le bas meme sous un `lte`, applique l'arrondi haut a
**chaque** operateur `/`, ramene le 31 janvier `+1M` au 28 fevrier, et remplit
les champs d'heure manquants au maximum mais les champs de date manquants au
minimum. Ce script pose les memes bornes aux deux serveurs et compare les
documents rendus, puis les messages d'erreur des expressions malformees.

    python3 tests/compat/diff_datemath.py [ferrite_url] [es_url]

Outil de developpement : exige un Elasticsearch 8.x lance a cote (Docker).
"""
import sys

from elasticsearch import ApiError, Elasticsearch

FERRITE = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:9200"
ES = sys.argv[2] if len(sys.argv) > 2 else "http://localhost:9201"
INDEX = "compat_datemath"

MAPPINGS = {
    "properties": {
        "d": {"type": "date"},
        "f": {"type": "date", "format": "yyyy-MM-dd HH:mm:ss"},
        "e": {"type": "date", "format": "epoch_millis"},
        "k": {"type": "keyword"},
        "lignes": {
            "type": "nested",
            "properties": {"jalon": {"type": "date"}, "ref": {"type": "keyword"}},
        },
    }
}

# Des instants poses sur les bords : minuit, la derniere milliseconde d'un jour,
# d'un mois, d'une annee. Un arrondi qui se trompe d'une milliseconde change la
# reponse sur au moins un de ces documents.
INSTANTS = [
    "2025-12-31T23:59:59.999Z",
    "2026-01-01T00:00:00.000Z",
    "2026-02-28T12:00:00.000Z",
    "2026-03-14T23:59:59.999Z",
    "2026-03-15T00:00:00.000Z",
    "2026-03-15T11:59:59.999Z",
    "2026-03-15T12:00:00.000Z",
    "2026-03-15T12:00:00.999Z",
    "2026-03-15T12:00:59.999Z",
    "2026-03-15T12:59:59.999Z",
    "2026-03-15T23:59:59.999Z",
    "2026-03-16T00:00:00.000Z",
    "2026-03-31T23:59:59.999Z",
    "2026-04-01T00:00:00.000Z",
    "2026-12-31T23:59:59.999Z",
]


def iso(ms):
    import datetime

    return (
        datetime.datetime.fromtimestamp(ms / 1000, datetime.timezone.utc)
        .strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3]
        + "Z"
    )


def millis(texte):
    import datetime

    dt = datetime.datetime.strptime(texte, "%Y-%m-%dT%H:%M:%S.%fZ")
    return int(dt.replace(tzinfo=datetime.timezone.utc).timestamp() * 1000)


def documents(maintenant):
    """Les instants fixes, plus des documents places autour de `now`.

    Les seconds sont ce qui donne du sens a `now-1d` : sans eux, toutes les
    expressions relatives rendraient le meme ensemble (tout ou rien) et le
    comparateur serait d'accord sans rien avoir mesure.
    """
    docs = []
    for i, t in enumerate(INSTANTS):
        ms = millis(t)
        docs.append(
            (
                f"i{i}",
                {
                    "d": t,
                    "f": t[:10] + " " + t[11:19],
                    "e": ms,
                    "k": f"i{i}",
                    "lignes": [
                        {"jalon": t, "ref": "a"},
                        {"jalon": "2020-01-01T00:00:00.000Z", "ref": "b"},
                    ],
                },
            )
        )
    # Des decalages qui encadrent les expressions relatives testees plus bas.
    for i, decalage in enumerate(
        [
            -400 * 86400_000,
            -40 * 86400_000,
            -8 * 86400_000,
            -36 * 3600_000,
            -90 * 60_000,
            -30_000,
            30_000,
            90 * 60_000,
            36 * 3600_000,
            8 * 86400_000,
            40 * 86400_000,
        ]
    ):
        ms = maintenant + decalage
        t = iso(ms)
        docs.append(
            (
                f"n{i}",
                {
                    "d": t,
                    "f": t[:10] + " " + t[11:19],
                    "e": ms,
                    "k": f"n{i}",
                    "lignes": [{"jalon": t, "ref": "a"}],
                },
            )
        )
    return docs


# Les expressions posees sur chacune des quatre bornes.
EXPRESSIONS = [
    # --- le mot-cle `now` et ses operations
    "now",
    "now-1d",
    "now+1d",
    "now-1h",
    "now-90m",
    "now-2H",
    "now-30s",
    "now-1w",
    "now-1M",
    "now-1y",
    "now/d",
    "now-1d/d",
    "now+1d/d",
    "now/h",
    "now/m",
    "now/s",
    "now/w",
    "now/M",
    "now/y",
    "now-1M/M",
    "now-1y/y",
    "now/d+12h",
    "now/d-1d",
    "now-1d/d+1h",
    "now+1d+1d",
    # --- une ancre explicite, puis des operations
    "2026-03-15||+1d",
    "2026-03-15||-1d",
    "2026-03-15T12:00:00Z||+1h",
    "2026-03-15T12:00:00.500Z||/d",
    "2026-03-15||/M",
    "2026-03-15||/y",
    "2026-03-15||/w",
    "2026-03-15||/M/d",
    "2026-01-31||+1M",
    "2026-03-31||-1M",
    "2026-03-16||-1d",
    "2026-03-16T10:00:00Z||/d-1d",
    "1773532800000||+1d",
    # --- pas de date math du tout : l'arrondi selon la borne
    "2026-03-15",
    "2026-03-15T12",
    "2026-03-15T12:00",
    "2026-03-15T12:00:00",
    "2026-03-15T12:00:00.500",
    "2026-03-15T12:00:00.500Z",
    "2026-03-15T12:00:00+02:00",
    "2026-03",
    "2026",
    "1773532800000",
]

# Les expressions qu'ES refuse : ferrite doit les refuser avec le meme message.
MALFORMEES = [
    "now-1q",
    "now/D",
    "now-1",
    "now/",
    "now-",
    "now+1d/",
    "nowX",
    "now1d",
    "now/dd",
    "now-1.5d",
    "now-99999999999999d",
    "2026-03-15||+1d||+1d",
    "||+1d",
    "NOW",
    "2026-03-15+1d",
    "pas-une-date",
    "2026-13-01",
    "2026-03-32",
]


def requetes(maintenant):
    q = []
    for expr in EXPRESSIONS:
        for borne in ("gte", "gt", "lte", "lt"):
            q.append(
                (f"range d {borne} [{expr}]", {"range": {"d": {borne: expr}}})
            )
    # Le meme champ sous un format declare, et sous `epoch_millis`.
    for expr in ("now", "now-1d/d", "2026-03-15 12:00:00", "2026-03-15 12:00:00||/d"):
        for borne in ("gte", "lte"):
            q.append((f"range f {borne} [{expr}]", {"range": {"f": {borne: expr}}}))
    for expr in ("now", "now/d", "1773532800000"):
        for borne in ("gte", "lte"):
            q.append((f"range e {borne} [{expr}]", {"range": {"e": {borne: expr}}}))
    # Un intervalle complet, comme un vrai filtre de KPI.
    q.append(
        (
            "range d entre now-1M/M et now",
            {"range": {"d": {"gte": "now-1M/M", "lt": "now"}}},
        )
    )
    q.append(
        (
            "bool filter range now (le filtre KPI)",
            {
                "bool": {
                    "filter": [{"range": {"d": {"lt": "now"}}}],
                    "must_not": [{"range": {"d": {"lt": "now-1y"}}}],
                }
            },
        )
    )
    # `format` : le format du champ remplace pour lire les bornes.
    for expr, fmt in (
        ("15/03/2026", "dd/MM/yyyy"),
        ("15/03/2026||+1d", "dd/MM/yyyy"),
        ("2026-03-15 23:59:59", "yyyy-MM-dd HH:mm:ss"),
        ("now", "dd/MM/yyyy"),
    ):
        for borne in ("gte", "lte", "lt"):
            q.append(
                (
                    f"range d {borne} [{expr}] format={fmt}",
                    {"range": {"d": {borne: expr, "format": fmt}}},
                )
            )
    # `term` / `terms` / `match` : une date y designe une periode, pas un instant.
    for expr in (
        "2026-03-15",
        "2026-03-15T12:00:00.000Z",
        "2026-03-14||+1d",
        "2026-03-15||/d",
        "now/d",
        "now",
        "2026-03",
    ):
        q.append((f"term d [{expr}]", {"term": {"d": expr}}))
        q.append((f"match d [{expr}]", {"match": {"d": expr}}))
        q.append((f"terms d [{expr}]", {"terms": {"d": [expr, "2026-04-01"]}}))
    # Sous un `nested`, ou ferrite verifie element par element.
    for expr in ("now", "now-1d/d", "2026-03-15||/d", "2026-03-15"):
        for borne in ("gte", "lt", "lte"):
            q.append(
                (
                    f"nested lignes.jalon {borne} [{expr}]",
                    {
                        "nested": {
                            "path": "lignes",
                            "query": {
                                "bool": {
                                    "must": [
                                        {"range": {"lignes.jalon": {borne: expr}}},
                                        {"term": {"lignes.ref": "a"}},
                                    ]
                                }
                            },
                        }
                    },
                )
            )
        q.append(
            (
                f"nested term lignes.jalon [{expr}]",
                {
                    "nested": {
                        "path": "lignes",
                        "query": {"term": {"lignes.jalon": expr}},
                    }
                },
            )
        )
    # Un champ qui n'est pas une date : `now` y est une chaine ordinaire.
    q.append(("range k lt [now] (keyword)", {"range": {"k": {"lt": "now"}}}))
    for expr in MALFORMEES:
        q.append((f"malformee [{expr}]", {"range": {"d": {"lt": expr}}}))
    return q


class Comparateur:
    def __init__(self):
        self.ferrite = Elasticsearch(FERRITE, request_timeout=60)
        self.es = Elasticsearch(ES, request_timeout=60)
        self.total = 0
        self.identiques = 0
        self.refus_communs = 0
        self.messages_identiques = 0
        self.ecarts = []

    def prepare(self, docs):
        for client, nom in ((self.ferrite, "ferrite"), (self.es, "ES")):
            client.options(ignore_status=404).indices.delete(index=INDEX)
            client.indices.create(
                index=INDEX,
                mappings=MAPPINGS,
                settings={"number_of_shards": 1, "number_of_replicas": 0},
            )
            ops = []
            for doc_id, doc in docs:
                ops.append({"index": {"_index": INDEX, "_id": doc_id}})
                ops.append(doc)
            r = client.bulk(operations=ops)
            if r.get("errors"):
                premier = next(
                    item["index"]["error"]
                    for item in r["items"]
                    if item["index"].get("error")
                )
                print(f"   {nom:<8} refus a l'indexation : {premier}")
            client.indices.refresh(index=INDEX)
            n = client.search(index=INDEX, query={"match_all": {}}, size=0)["hits"][
                "total"
            ]["value"]
            print(f"   {nom:<8} {n} documents indexes")

    def _cherche(self, client, query):
        try:
            r = client.search(index=INDEX, query=query, size=100, sort=["k"])
            return [h["_id"] for h in r["hits"]["hits"]], None
        except ApiError as exc:
            # ES groupe l'echec sous `search_phase_execution_exception` : la
            # cause est ce que le client affiche, et c'est elle qui compte.
            racine = (exc.body.get("error") or {}).get("root_cause") or [{}]
            return None, (racine[0].get("type"), racine[0].get("reason"))

    def compare(self, label, query):
        self.total += 1
        idf, erreur_f = self._cherche(self.ferrite, query)
        ide, erreur_e = self._cherche(self.es, query)

        if erreur_e is not None:
            if erreur_f is None:
                self.ecarts.append(
                    (
                        label,
                        f"ES refuse ({erreur_e[0]}), ferrite rend {len(idf)} documents",
                    )
                )
                return
            self.refus_communs += 1
            self.identiques += 1
            if erreur_f == erreur_e:
                self.messages_identiques += 1
            elif erreur_f[0] != erreur_e[0]:
                self.ecarts.append(
                    (
                        label,
                        f"refus des deux cotes, type different :\n"
                        f"          ferrite : {erreur_f[0]} : {erreur_f[1]}\n"
                        f"          ES      : {erreur_e[0]} : {erreur_e[1]}",
                    )
                )
            else:
                print(
                    f"  [message] {label}\n"
                    f"          ferrite : {erreur_f[1]}\n"
                    f"          ES      : {erreur_e[1]}"
                )
            return
        if erreur_f is not None:
            self.ecarts.append(
                (
                    label,
                    f"ferrite refuse ({erreur_f[0]} : {erreur_f[1]}), "
                    f"ES rend {len(ide)} documents",
                )
            )
            return

        if idf == ide:
            self.identiques += 1
            return
        manque = sorted(set(ide) - set(idf))
        trop = sorted(set(idf) - set(ide))
        self.ecarts.append(
            (
                label,
                f"{len(idf)} vs {len(ide)} documents ; "
                f"manquants={manque[:8]} en trop={trop[:8]}",
            )
        )

    def run(self):
        import time

        print(f"== ferrite : {FERRITE}\n== ES      : {ES}\n")
        maintenant = int(time.time() * 1000)
        docs = documents(maintenant)
        print(f"== indexation de {len(docs)} documents")
        self.prepare(docs)

        qs = requetes(maintenant)
        print(f"\n== {len(qs)} bornes, posees aux deux serveurs\n")
        for label, query in qs:
            self.compare(label, query)

        for label, detail in self.ecarts:
            print(f"  [ecart] {label}\n          {detail}")
        print()
        print(
            f"  {self.identiques}/{self.total} bornes : memes documents "
            f"(dont {self.refus_communs} refusees des deux cotes, "
            f"{self.messages_identiques} au message pres)"
        )
        return 1 if self.ecarts else 0


if __name__ == "__main__":
    sys.exit(Comparateur().run())
