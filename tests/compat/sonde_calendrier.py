#!/usr/bin/env python3
"""Sonde : le calendrier — `calendar_interval`, `time_zone`, et les seaux d'un
graphe temporel, seau par seau.

    python3 tests/compat/sonde_calendrier.py [ferrite] [es]
    python3 tests/compat/sonde_calendrier.py --calibrer [es_a] [es_b]

Un mois n'est pas trente jours, et un jour n'est pas toujours vingt-quatre
heures : le dernier dimanche de mars, a Paris, il en dure 23 — et celui
d'octobre 25. Un `fixed_interval` ne sait dire ni l'un ni l'autre. Cette sonde
mesure ce que ferrite en fait, contre un vrai Elasticsearch 8.15, sur un corpus
bati **pour les endroits ou c'est difficile** :

- les deux bascules de 2026 a Paris, a la minute et a la milliseconde pres,
  dans les deux sens ;
- un 29 fevrier, et le 1er mars qui le suit ;
- une zone dont **minuit n'existe pas** ce jour-la (`America/Santiago`, le
  8 septembre 2024 commence a 01:00) ;
- une zone a decalage non entier (`Asia/Kolkata`, +05:30) et une a heure d'ete
  d'une demi-heure (`Australia/Lord_Howe`) ;
- des documents poses **exactement** sur une frontiere de seau — minuit, le
  premier du mois — parce que c'est le cas courant d'un graphe temporel et
  celui ou une borne arrondie du mauvais cote se voit.

Ce qui est compare n'est pas le nombre de seaux : c'est le **bloc entier**,
seau par seau, `key`, `key_as_string` et `doc_count` compris, sous-agregations
comprises. Un `key_as_string` faux (`Z` la ou ES ecrit `+01:00`) est un graphe
faux dont l'axe ment, et il ne change aucun compte.

La sonde mesure aussi `time_zone` sur la **requete** `range` : c'est la meme
mecanique — une borne arrondie dans un fuseau — et c'etait le meme refus.

`--calibrer` rejoue la batterie contre deux Elasticsearch. Tant qu'elle n'y est
pas a zero, ce qu'elle dit de ferrite ne vaut rien.
"""
import json
import sys
import urllib.error
import urllib.request

INDEX = "sonde-calendrier"

# Ce que ferrite refuse **exprès** la ou ES repond, avec la mesure qui justifie
# le refus.
REFUS_ASSUMES = {
    "order _key desc":
        "ES classe les seaux d'un `date_histogram` par `order` ; ferrite ne "
        "sert que l'ordre chronologique (cout de perimetre declare, "
        "docs/compat.md)",
    "order _count desc":
        "meme chose : l'ordre par compte n'est pas servi",
}


# ---------------------------------------------------------------------------
# Le corpus : des instants places la ou l'arrondi est difficile


def corpus():
    """Les documents, dans un ordre fixe : un ecart doit se rejouer."""
    docs = []

    def ajoute(iso, k, n):
        docs.append({"d": iso, "k": k, "n": n})

    # -- la bascule de printemps a Paris (2026-03-29T01:00Z) ----------------
    for i, iso in enumerate([
        "2026-03-28T22:30:00.000Z", "2026-03-28T23:00:00.000Z",
        "2026-03-29T00:00:00.000Z", "2026-03-29T00:59:59.999Z",
        "2026-03-29T01:00:00.000Z", "2026-03-29T01:00:00.001Z",
        "2026-03-29T01:30:00.000Z", "2026-03-29T02:30:00.000Z",
        "2026-03-29T21:59:59.999Z", "2026-03-29T22:00:00.000Z",
        "2026-03-30T05:00:00.000Z",
    ]):
        ajoute(iso, "printemps", i)

    # -- la bascule d'automne (2026-10-25T01:00Z), ou une heure locale
    #    existe deux fois ------------------------------------------------
    for i, iso in enumerate([
        "2026-10-24T21:59:59.999Z", "2026-10-24T22:00:00.000Z",
        "2026-10-25T00:00:00.000Z", "2026-10-25T00:30:00.000Z",
        "2026-10-25T00:59:59.999Z", "2026-10-25T01:00:00.000Z",
        "2026-10-25T01:30:00.000Z", "2026-10-25T02:00:00.000Z",
        "2026-10-25T22:59:59.999Z", "2026-10-25T23:00:00.000Z",
    ]):
        ajoute(iso, "automne", i)

    # -- un 29 fevrier, et les bords de mois / trimestre / annee -----------
    for i, iso in enumerate([
        "2024-02-29T00:00:00.000Z", "2024-02-29T12:00:00.000Z",
        "2024-02-29T23:59:59.999Z", "2024-03-01T00:00:00.000Z",
        "2024-01-01T00:00:00.000Z", "2024-04-01T00:00:00.000Z",
        "2024-06-30T23:59:59.999Z", "2024-07-01T00:00:00.000Z",
        "2024-12-31T23:59:59.999Z", "2025-01-01T00:00:00.000Z",
    ]):
        ajoute(iso, "bords", i)

    # -- le jour ou minuit n'existe pas a Santiago (2024-09-08) ------------
    for i, iso in enumerate([
        "2024-09-07T20:00:00.000Z", "2024-09-08T03:59:59.999Z",
        "2024-09-08T04:00:00.000Z", "2024-09-08T12:00:00.000Z",
        "2024-09-09T02:59:59.999Z",
    ]):
        ajoute(iso, "santiago", i)

    # -- Lord Howe : une heure d'ete d'une demi-heure (2026-04-05) ---------
    for i, iso in enumerate([
        "2026-04-04T14:00:00.000Z", "2026-04-04T15:00:00.000Z",
        "2026-04-04T16:00:00.000Z", "2026-10-03T15:00:00.000Z",
        "2026-10-03T16:00:00.000Z",
    ]):
        ajoute(iso, "lordhowe", i)

    # -- des minuits pile, dans plusieurs fuseaux : le cas ou une borne
    #    arrondie du mauvais cote deplace un document de seau -------------
    for i, iso in enumerate([
        "2026-05-31T22:00:00.000Z",  # 1er juin a minuit a Paris
        "2026-05-31T18:30:00.000Z",  # 1er juin a minuit a Kolkata
        "2026-06-01T00:00:00.000Z",  # 1er juin a minuit UTC
        "2026-06-01T04:00:00.000Z",  # 1er juin a minuit a New York
        "2026-06-30T22:00:00.000Z", "2026-06-30T18:30:00.000Z",
        "2026-07-01T00:00:00.000Z",
    ]):
        ajoute(iso, "minuits", i)

    # -- des documents poses a la milliseconde, sur les frontieres d'un
    #    intervalle sous-seconde : c'est la seule forme ou la borne passee a
    #    tantivy (un flottant de nanosecondes) ne represente pas exactement la
    #    milliseconde demandee -----------------------------------------------
    for i in range(16):
        ajoute(f"2026-03-29T01:00:00.{i * 62 % 1000:03d}Z", "millisecondes", i)

    # -- une serie reguliere, pour que les seaux vides existent -----------
    for i in range(1, 25):
        ajoute(f"2025-{i % 12 + 1:02d}-{(i % 27) + 1:02d}T08:15:30.250Z", "serie", i)

    # -- deux documents **sans date** : sous un `terms`, leur categorie n'a
    #    aucun seau a montrer, et c'est le seul cas ou le rognage par seau
    #    parent n'a rien a rogner ------------------------------------------
    docs.append({"k": "sansdate", "n": 90})
    docs.append({"k": "sansdate", "n": 91})

    return docs


DOCS = corpus()

ZONES = [None, "UTC", "Europe/Paris", "America/New_York", "Asia/Kolkata",
         "America/Santiago", "Australia/Lord_Howe", "+05:30", "-08:00"]

UNITES = ["second", "minute", "hour", "day", "week", "month", "quarter", "year"]


def questions():
    """(label, corps de recherche) — l'ordre est fixe."""
    q = []

    def ajoute(label, agg, requete=None, extra=None):
        corps = {"size": 0, "aggs": {"h": agg}}
        if requete:
            corps["query"] = requete
        if extra:
            corps.update(extra)
        q.append((label, corps))

    # -- chaque unite de calendrier, dans chaque fuseau ------------------
    for unite in UNITES:
        for zone in ZONES:
            # Les unites fines feraient des millions de seaux sur trois ans :
            # on les pose sur une fenetre etroite.
            requete = None
            if unite in ("second", "minute", "hour"):
                requete = {"range": {"d": {"gte": "2026-03-28T20:00:00Z",
                                           "lte": "2026-03-30T06:00:00Z"}}}
            agg = {"field": "d", "calendar_interval": unite}
            if zone:
                agg["time_zone"] = zone
            ajoute(f"calendar {unite} tz={zone}", {"date_histogram": agg}, requete)

    # -- les intervalles fixes, avec fuseau : un seau « fixe » ne l'est
    #    plus quand il traverse une bascule ----------------------------
    for fixe in ["30m", "1h", "3h", "12h", "1d", "7d", "90m"]:
        for zone in [None, "Europe/Paris", "Asia/Kolkata", "Australia/Lord_Howe"]:
            agg = {"field": "d", "fixed_interval": fixe}
            if zone:
                agg["time_zone"] = zone
            requete = None
            if fixe in ("30m", "1h", "90m"):
                requete = {"range": {"d": {"gte": "2026-10-24T20:00:00Z",
                                           "lte": "2026-10-26T06:00:00Z"}}}
            ajoute(f"fixed {fixe} tz={zone}", {"date_histogram": agg}, requete)

    # -- les intervalles sous-seconde : les seules bornes qu'un flottant de
    #    nanosecondes ne represente pas exactement --------------------------
    fenetre = {"range": {"d": {"gte": "2026-03-29T01:00:00.000Z",
                               "lte": "2026-03-29T01:00:01.000Z"}}}
    for fixe in ["1ms", "3ms", "62ms", "125ms", "250ms", "500ms"]:
        for zone in [None, "Europe/Paris"]:
            agg = {"field": "d", "fixed_interval": fixe}
            if zone:
                agg["time_zone"] = zone
            ajoute(f"fixed {fixe} tz={zone}", {"date_histogram": agg}, fenetre)
    ajoute("fixed 250ms + offset 3ms", {"date_histogram": {
        "field": "d", "fixed_interval": "250ms", "offset": "3ms"}}, fenetre)

    # -- offset, seul et combine a un fuseau ----------------------------
    for offset in ["+6h", "-2h", "+30m", "1d"]:
        for zone in [None, "Europe/Paris"]:
            agg = {"field": "d", "calendar_interval": "day", "offset": offset}
            if zone:
                agg["time_zone"] = zone
            ajoute(f"offset {offset} tz={zone}", {"date_histogram": agg})
    ajoute("offset sur un mois", {"date_histogram": {
        "field": "d", "calendar_interval": "month", "offset": "+15d",
        "time_zone": "Europe/Paris"}})

    # -- min_doc_count, extended_bounds, hard_bounds --------------------
    for n in [0, 1, 2, 3]:
        ajoute(f"min_doc_count {n}", {"date_histogram": {
            "field": "d", "calendar_interval": "month", "min_doc_count": n}})
    for bornes in [
        {"min": "2024-01-01", "max": "2027-01-01"},
        {"min": "2026-03-01T00:00:00", "max": "2026-04-01T00:00:00"},
        {"min": "2026-03-15T13:00:00", "max": "2026-03-20T09:00:00"},
        {"min": "2025-06-01"},
        {"max": "2027-06-01"},
    ]:
        ajoute(f"extended_bounds {json.dumps(bornes)}", {"date_histogram": {
            "field": "d", "calendar_interval": "month",
            "extended_bounds": bornes}})
        ajoute(f"extended_bounds+tz {json.dumps(bornes)}", {"date_histogram": {
            "field": "d", "calendar_interval": "month", "time_zone": "Europe/Paris",
            "extended_bounds": bornes}})
    for bornes in [
        {"min": "2026-03-01", "max": "2026-05-01"},
        {"min": "2026-03-29", "max": "2026-03-30"},
        {"min": "2026-03-15T13:00:00", "max": "2026-10-25T09:00:00"},
        {"min": "2026-01-01"},
        {"max": "2025-01-01"},
    ]:
        ajoute(f"hard_bounds {json.dumps(bornes)}", {"date_histogram": {
            "field": "d", "calendar_interval": "day", "hard_bounds": bornes}})
        ajoute(f"hard_bounds+tz {json.dumps(bornes)}", {"date_histogram": {
            "field": "d", "calendar_interval": "day", "time_zone": "Europe/Paris",
            "hard_bounds": bornes}})
    ajoute("extended_bounds en millisecondes", {"date_histogram": {
        "field": "d", "calendar_interval": "month",
        "extended_bounds": {"min": 1704067200000, "max": 1798761600000}}})
    ajoute("extended_bounds en date math", {"date_histogram": {
        "field": "d", "calendar_interval": "month",
        "extended_bounds": {"min": "2026-03-15||-2M/M", "max": "2026-03-15||+2M/M"}}})
    # Les bornes se lisent-elles dans le fuseau ? Ces deux-la sont ecrites
    # **juste avant** un debut de seau local : lues en UTC, elles designeraient
    # le seau suivant.
    ajoute("extended_bounds lue dans le fuseau", {"date_histogram": {
        "field": "d", "calendar_interval": "day", "time_zone": "Europe/Paris",
        "extended_bounds": {"min": "2026-02-28T23:30:00", "max": "2026-03-10"}}})
    ajoute("hard_bounds lue dans le fuseau", {"date_histogram": {
        "field": "d", "calendar_interval": "day", "time_zone": "Europe/Paris",
        "hard_bounds": {"min": "2026-03-01T00:30:00", "max": "2026-04-10"}}})
    ajoute("extended_bounds lue dans un fuseau a la demi-heure", {"date_histogram": {
        "field": "d", "calendar_interval": "day", "time_zone": "Asia/Kolkata",
        "extended_bounds": {"min": "2026-02-28T18:45:00", "max": "2026-03-10"}}})
    ajoute("hard_bounds dans extended_bounds", {"date_histogram": {
        "field": "d", "calendar_interval": "day",
        "hard_bounds": {"min": "2026-03-01", "max": "2026-05-01"},
        "extended_bounds": {"min": "2026-03-10", "max": "2026-04-10"}}})

    # -- keyed et format ------------------------------------------------
    for zone in [None, "Europe/Paris", "+05:30"]:
        agg = {"field": "d", "calendar_interval": "month", "keyed": True}
        if zone:
            agg["time_zone"] = zone
        ajoute(f"keyed tz={zone}", {"date_histogram": agg})
    for fmt in ["yyyy-MM-dd", "yyyy-MM", "epoch_millis", "epoch_second",
                "yyyy-MM-dd'T'HH:mm:ss", "date_optional_time"]:
        ajoute(f"format {fmt}", {"date_histogram": {
            "field": "d", "calendar_interval": "month", "format": fmt,
            "time_zone": "Europe/Paris"}})
    ajoute("format + keyed", {"date_histogram": {
        "field": "d", "calendar_interval": "month", "format": "yyyy-MM",
        "keyed": True, "time_zone": "America/New_York"}})

    # -- sous-agregations, et sous un seau parent -----------------------
    ajoute("sous-agregations", {"date_histogram": {
        "field": "d", "calendar_interval": "month", "time_zone": "Europe/Paris"},
        "aggs": {"m": {"avg": {"field": "n"}}, "s": {"stats": {"field": "n"}},
                 "t": {"terms": {"field": "k"}}}})
    ajoute("sous un terms", {"terms": {"field": "k", "size": 10},
                             "aggs": {"d": {"date_histogram": {
                                 "field": "d", "calendar_interval": "month",
                                 "time_zone": "Europe/Paris"}}}})
    ajoute("sous un terms, jour", {"terms": {"field": "k", "size": 10},
                                   "aggs": {"d": {"date_histogram": {
                                       "field": "d", "calendar_interval": "day",
                                       "time_zone": "Europe/Paris"}}}})
    ajoute("sous un terms, avec extended_bounds", {
        "terms": {"field": "k", "size": 10},
        "aggs": {"d": {"date_histogram": {
            "field": "d", "calendar_interval": "month",
            "extended_bounds": {"min": "2024-01-01", "max": "2027-01-01"}}}}})
    ajoute("sous un terms, categorie sans date + extended_bounds", {
        "terms": {"field": "k", "size": 10},
        "aggs": {"d": {"date_histogram": {
            "field": "d", "calendar_interval": "month",
            "extended_bounds": {"min": "2026-01-01", "max": "2026-04-01"}}}}})
    ajoute("sous un terms, categorie sans date + extended_bounds min seule", {
        "terms": {"field": "k", "size": 10},
        "aggs": {"d": {"date_histogram": {
            "field": "d", "calendar_interval": "month",
            "extended_bounds": {"min": "2026-01-01"}}}}})
    ajoute("sous un terms, categorie sans date + extended_bounds max seule", {
        "terms": {"field": "k", "size": 10},
        "aggs": {"d": {"date_histogram": {
            "field": "d", "calendar_interval": "month",
            "extended_bounds": {"max": "2026-04-01"}}}}})
    ajoute("sous un terms, categorie sans date", {
        "terms": {"field": "k", "size": 10},
        "aggs": {"d": {"date_histogram": {
            "field": "d", "calendar_interval": "month"}}}})
    ajoute("sous une filter", {"filter": {"term": {"k": "printemps"}},
                               "aggs": {"d": {"date_histogram": {
                                   "field": "d", "calendar_interval": "hour",
                                   "time_zone": "Europe/Paris"}}}})
    ajoute("date_histogram sur range", {"date_histogram": {
        "field": "d", "calendar_interval": "month", "time_zone": "Europe/Paris"},
        "aggs": {"r": {"range": {"field": "n", "ranges": [
            {"to": 5}, {"from": 5}]}}}})

    # -- les cas ou il n'y a rien a montrer ------------------------------
    ajoute("aucun document", {"date_histogram": {
        "field": "d", "calendar_interval": "day"}}, {"match_none": {}})
    ajoute("aucun document + extended_bounds", {"date_histogram": {
        "field": "d", "calendar_interval": "month", "time_zone": "Europe/Paris",
        "extended_bounds": {"min": "2026-01-01", "max": "2026-06-01"}}},
        {"match_none": {}})
    ajoute("un seul document", {"date_histogram": {
        "field": "d", "calendar_interval": "year", "time_zone": "Europe/Paris"}},
        {"term": {"n": 0, }} if False else {"ids": {"values": ["0"]}})

    # -- les refus qu'on attend -----------------------------------------
    ajoute("order _key desc", {"date_histogram": {
        "field": "d", "calendar_interval": "month", "order": {"_key": "desc"}}})
    ajoute("order _count desc", {"date_histogram": {
        "field": "d", "calendar_interval": "month", "order": {"_count": "desc"}}})

    # -- ce que les deux doivent refuser de la meme facon ---------------
    for label, agg in [
        ("calendar 2d", {"field": "d", "calendar_interval": "2d"}),
        ("calendar 90m", {"field": "d", "calendar_interval": "90m"}),
        ("fixed 1M", {"field": "d", "fixed_interval": "1M"}),
        ("fixed 0s", {"field": "d", "fixed_interval": "0s"}),
        ("les deux intervalles", {"field": "d", "calendar_interval": "1d",
                                  "fixed_interval": "1d"}),
        ("aucun intervalle", {"field": "d"}),
        ("fuseau inconnu", {"field": "d", "calendar_interval": "day",
                            "time_zone": "Europe/Nulle_Part"}),
        ("hard_bounds inverses", {"field": "d", "calendar_interval": "day",
                                  "hard_bounds": {"min": "2026-05-01",
                                                  "max": "2026-01-01"}}),
        ("extended hors hard", {"field": "d", "calendar_interval": "day",
                                "hard_bounds": {"min": "2026-03-01", "max": "2026-04-01"},
                                "extended_bounds": {"min": "2026-01-01", "max": "2026-06-01"}}),
        ("champ non date", {"field": "k", "calendar_interval": "day"}),
    ]:
        ajoute(f"refus {label}", {"date_histogram": agg})

    return q


def questions_range():
    """`time_zone` sur la requete `range` : la meme mecanique, le meme refus."""
    q = []
    for zone in ["Europe/Paris", "Asia/Kolkata", "America/New_York", "+05:30",
                 "-08:00", "UTC"]:
        for bornes in [
            {"gte": "2026-03-29", "lt": "2026-03-30"},
            {"gte": "2026-03-29T00:00:00", "lte": "2026-03-29T23:59:59"},
            {"lte": "2026-03-29"},
            {"gt": "2026-10-25"},
            {"gte": "2024-02-29", "lte": "2024-02-29"},
            {"gte": "now-100y", "lte": "now+100y"},
            {"gte": "2026-06-01||/M", "lt": "2026-06-01||+1M/M"},
            {"gte": "2026-03-29T02:30:00"},
        ]:
            corps = dict(bornes)
            corps["time_zone"] = zone
            q.append((f"range {json.dumps(bornes)} tz={zone}",
                      {"size": 100, "sort": ["n"], "_source": ["d", "k", "n"],
                       "query": {"range": {"d": corps}}}))
    # Un fuseau invalide doit etre refuse des deux cotes.
    q.append(("range fuseau inconnu", {"size": 0, "query": {"range": {"d": {
        "gte": "2026-01-01", "time_zone": "Europe/Nulle_Part"}}}}))
    # Sur un champ qui n'est pas une date, ES refuse `time_zone`.
    q.append(("range time_zone sur un entier", {"size": 0, "query": {"range": {
        "n": {"gte": 1, "time_zone": "Europe/Paris"}}}}))
    return q


# ---------------------------------------------------------------------------


def http(base, method, path, body=None, ndjson=None):
    if ndjson is not None:
        data, ct = ndjson.encode(), "application/x-ndjson"
    else:
        data = json.dumps(body).encode() if body is not None else None
        ct = "application/json"
    req = urllib.request.Request(base + path, data=data, method=method,
                                 headers={"Content-Type": ct})
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        brut = e.read()
        try:
            return e.code, json.loads(brut or b"{}")
        except json.JSONDecodeError:
            return e.code, {"raw": brut.decode(errors="replace")}


MARQUEUR_FERRITE = "not_implemented_in_ferrite_exception"


def prepare(base):
    http(base, "DELETE", f"/{INDEX}")
    statut, corps = http(base, "PUT", f"/{INDEX}", {
        "mappings": {"properties": {
            "d": {"type": "date"}, "k": {"type": "keyword"}, "n": {"type": "long"}}}})
    if statut >= 400:
        print(f"[{base}] ne prend pas l'index de la sonde : {corps}", file=sys.stderr)
        sys.exit(2)
    lignes = []
    for i, doc in enumerate(DOCS):
        lignes.append(json.dumps({"index": {"_id": str(i)}}))
        lignes.append(json.dumps(doc))
    statut, corps = http(base, "POST", f"/{INDEX}/_bulk?refresh=true",
                         ndjson="\n".join(lignes) + "\n")
    if statut >= 400 or corps.get("errors"):
        print(f"[{base}] refuse le corpus : {json.dumps(corps)[:400]}", file=sys.stderr)
        sys.exit(2)


def reponse(base, corps):
    statut, brut = http(base, "POST", f"/{INDEX}/_search", corps)
    if statut != 200:
        err = brut.get("error", {}) if isinstance(brut, dict) else {}
        cause = (err.get("root_cause") or [err])[0]
        return statut, cause.get("type"), str(cause.get("reason", ""))[:200], None
    if "aggregations" in brut:
        return statut, None, "", brut["aggregations"]
    hits = brut.get("hits", {})
    return statut, None, "", {
        "total": hits.get("total", {}).get("value"),
        "ids": [h["_id"] for h in hits.get("hits", [])],
    }


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    calibrer = "--calibrer" in sys.argv
    defauts = ("http://localhost:9201", "http://localhost:9202") if calibrer else (
        "http://localhost:9200", "http://localhost:9201")
    a = args[0] if args else defauts[0]
    b = args[1] if len(args) > 1 else defauts[1]
    noms = ("es_a", "es_b") if calibrer else ("ferrite", "es")
    for base, nom in zip((a, b), noms):
        statut, _ = http(base, "GET", "/")
        if statut != 200:
            print(f"[{nom}] ne repond pas sur {base} — une sonde qui ne trouve "
                  "qu'un serveur ne compare rien", file=sys.stderr)
            return 2

    prepare(a)
    prepare(b)

    cas = [("agg", label, corps) for label, corps in questions()]
    cas += [("range", label, corps) for label, corps in questions_range()]

    identiques = refus = ecarts = 0
    detail = []
    for famille, label, corps in cas:
        sa, ta, ma, ra = reponse(a, corps)
        sb, tb, mb, rb = reponse(b, corps)
        if ta == MARQUEUR_FERRITE and label.replace("refus ", "") in REFUS_ASSUMES:
            refus += 1
            print(f"refus {label}\n      "
                  f"{REFUS_ASSUMES[label.replace('refus ', '')]}")
            continue
        # Un refus des deux cotes : ce qui doit coincider est le **statut**,
        # pas la phrase — les deux serveurs n'ecrivent pas dans la meme langue.
        if sa >= 400 or sb >= 400:
            if sa == sb:
                identiques += 1
                print(f"ok    {label} (refuse des deux cotes, {sa})")
            else:
                ecarts += 1
                detail.append((label, f"{noms[0]} {sa} {ta} {ma}",
                               f"{noms[1]} {sb} {tb} {mb}"))
                print(f"ECART {label}")
                print(f"      {noms[0]} {sa} {ta} {ma[:150]}")
                print(f"      {noms[1]} {sb} {tb} {mb[:150]}")
            continue
        if ra == rb:
            identiques += 1
            print(f"ok    {label}")
            continue
        ecarts += 1
        ja, jb = json.dumps(ra, sort_keys=True), json.dumps(rb, sort_keys=True)
        detail.append((label, ja[:400], jb[:400]))
        print(f"ECART {label}")
        print(f"      {noms[0]} {premiere_difference(ra, rb)}")
        print(f"      {noms[0]} {ja[:300]}")
        print(f"      {noms[1]} {jb[:300]}")

    for base in (a, b):
        http(base, "DELETE", f"/{INDEX}")

    total = identiques + refus + ecarts
    print(f"\n{identiques}/{total} identiques, {refus} refus assumes, "
          f"{ecarts} ecarts — {len(DOCS)} documents, "
          f"{len(cas)} questions")
    return 1 if ecarts else 0


def premiere_difference(a, b, chemin=""):
    """Ou les deux reponses divergent, en une ligne : un bloc de seaux entier
    ne se lit pas."""
    if isinstance(a, dict) and isinstance(b, dict):
        for cle in sorted(set(a) | set(b)):
            if cle not in a:
                return f"{chemin}.{cle} manque a gauche"
            if cle not in b:
                return f"{chemin}.{cle} en trop a gauche"
            if a[cle] != b[cle]:
                return premiere_difference(a[cle], b[cle], f"{chemin}.{cle}")
        return chemin or "identiques ?"
    if isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            return f"{chemin} : {len(a)} elements a gauche, {len(b)} a droite"
        for i, (x, y) in enumerate(zip(a, b)):
            if x != y:
                return premiere_difference(x, y, f"{chemin}[{i}]")
    return f"{chemin} : {json.dumps(a)[:90]} != {json.dumps(b)[:90]}"


if __name__ == "__main__":
    sys.exit(main())
