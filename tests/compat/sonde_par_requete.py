#!/usr/bin/env python3
"""Sonde : que comptent *vraiment* `_delete_by_query` et `_update_by_query` ?

    python3 tests/compat/sonde_par_requete.py [ferrite] [es]

Ces deux routes rendent une dizaine de compteurs, et aucun n'est devinable a la
lecture de la documentation. Ce que cette sonde a servi a etablir :

- `total` vaut `min(correspondants, max_docs)` — et il **ne diminue pas** quand
  la commande s'interrompt sur un conflit. Un `total: 6, deleted: 1` est donc
  une reponse normale, pas une incoherence ;
- `batches` est le nombre de lots **reellement traites** : `ceil(traites /
  scroll_size)`. Il s'arrete la ou la commande s'arrete, alors que `total` non ;
- `conflicts=abort` s'arrete a la fin du **lot** fautif, pas au document : sur
  six documents en lots de deux, un conflit sur le cinquieme laisse le sixieme
  supprime ;
- `_update_by_query` **sans corps** vaut `match_all` ; `_delete_by_query` sans
  `query` rend 400 `query is missing`. La meme omission, deux reponses
  opposees — et c'est la bonne asymetrie : une purge par distraction n'arrive
  pas ;
- `refresh=wait_for` y est **refuse** (400), alors que `_doc` et `_bulk`
  l'acceptent ;
- la reponse de `_delete_by_query` ne porte pas de cle `updated` du tout.

Ce qui se compare ici n'est pas seulement la reponse : c'est aussi **l'etat
laisse derriere**. Une commande qui rend les bons compteurs en supprimant les
mauvais documents serait verte sur les compteurs seuls. Chaque cas compare donc
le corps de la reponse (`took` et les compteurs de regulation mis a part) *et*
la liste des documents restants, avec leur `_version` et leur `_source`.
"""
import json
import sys
import urllib.error
import urllib.request

INDEX = "sonde-par-requete"
INDEX_B = "sonde-par-requete-b"

MAPPING = {
    "settings": {"number_of_shards": 1, "number_of_replicas": 0},
    "mappings": {"properties": {
        "tenant": {"type": "keyword"},
        "n": {"type": "integer"},
        "txt": {"type": "text"},
        "paru": {"type": "date"},
    }},
}


def http(base, method, path, body=None, brut=None):
    data = brut if brut is not None else (
        json.dumps(body).encode() if body is not None else None)
    req = urllib.request.Request(
        base + path, data=data, method=method,
        headers={"Content-Type": "application/x-ndjson" if brut is not None
                 else "application/json"})
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        corps = e.read()
        try:
            return e.code, json.loads(corps or b"{}")
        except json.JSONDecodeError:
            return e.code, {"raw": corps.decode(errors="replace")}


def documents(base_id=0, n=6):
    return [(str(base_id + i),
             {"tenant": "a" if i % 2 == 0 else "b", "n": base_id + i,
              "txt": f"document numero {base_id + i}",
              "paru": f"2026-0{1 + i % 6}-15"})
            for i in range(n)]


def remplir(base, index=INDEX, base_id=0, n=6):
    http(base, "DELETE", f"/{index}")
    http(base, "PUT", f"/{index}", MAPPING)
    lignes = []
    for doc_id, doc in documents(base_id, n):
        lignes.append(json.dumps({"index": {"_index": index, "_id": doc_id}}))
        lignes.append(json.dumps(doc))
    http(base, "POST", "/_bulk?refresh=true", brut=("\n".join(lignes) + "\n").encode())


# ---------------------------------------------------------------------------
# Ce qui se compare
# ---------------------------------------------------------------------------

# Les compteurs de regulation : ferrite ne regule pas, ES non plus tant que
# `requests_per_second` n'est pas pose. Ils sont donc identiques — mais
# `throttled_millis` d'ES bouge des qu'une seconde de mur passe, et le comparer
# ferait clignoter la sonde sans rien mesurer.
VOLATILES = ("took", "throttled_millis", "throttled_until_millis")


def normalise(statut, corps):
    """La reponse telle qu'elle se compare : statut, compteurs, `failures[]`.

    Le `reason` d'un conflit cite le `seqNo` courant, qui n'a aucune raison de
    coincider entre deux moteurs : de chaque echec on garde le **type**, le
    document et le statut — c'est-a-dire tout ce qu'un client en fait.
    """
    if not isinstance(corps, dict):
        return statut, corps
    if "error" in corps:
        err = corps["error"]
        return statut, {"erreur": err.get("type") if isinstance(err, dict) else err}
    vu = {c: v for c, v in corps.items() if c not in VOLATILES}
    vu["failures"] = sorted(
        (f.get("index"), f.get("id"), (f.get("cause") or {}).get("type"), f.get("status"))
        for f in corps.get("failures", []))
    return statut, vu


def etat(base, index=INDEX):
    """Les documents restants : identifiant, `_version` et `_source`.

    La `_version` compte : `_update_by_query` doit la faire avancer d'un cran
    par document, meme quand il reecrit un `_source` identique. Sans elle, une
    commande qui ne ferait **rien** rendrait le meme etat qu'une commande qui
    reindexe tout. Elle se relit document par document : `version: true` dans le
    corps d'une recherche est un refus declare de ferrite, et le contourner ici
    mesurerait autre chose que ce qu'on veut mesurer.

    Un index absent rend `None` — et non le message d'erreur du serveur, qui
    differe d'un moteur a l'autre sans rien dire de la commande qu'on teste.
    """
    http(base, "POST", f"/{index}/_refresh")
    statut, corps = http(base, "POST", f"/{index}/_search", {"size": 100, "sort": ["n"]})
    if statut == 404:
        return None
    if statut != 200:
        return {"index illisible": f"{statut} {json.dumps(corps)[:120]}"}
    out = {}
    for hit in corps["hits"]["hits"]:
        _, doc = http(base, "GET", f"/{index}/_doc/{hit['_id']}")
        out[hit["_id"]] = (doc.get("_version"), doc.get("_source"))
    return out


# ---------------------------------------------------------------------------
# Les cas
# ---------------------------------------------------------------------------
#
# Un cas est (libelle, preparation, appels). `preparation` remet les deux
# serveurs dans le meme etat ; `appels` est la liste des requetes a poser, dont
# seule la **derniere** est comparee (les autres construisent l'etat).

def cas():
    tous = []

    def ajoute(libelle, prep, *appels):
        tous.append((libelle, prep, list(appels)))

    simple = lambda base: remplir(base)                       # noqa: E731
    deux = lambda base: (remplir(base), remplir(base, INDEX_B, 100, 4))  # noqa: E731

    # -- le geste nu -------------------------------------------------------
    ajoute("dbq, un terme", simple,
           ("POST", f"/{INDEX}/_delete_by_query?refresh=true",
            {"query": {"term": {"tenant": "a"}}}))
    ajoute("dbq, match_all", simple,
           ("POST", f"/{INDEX}/_delete_by_query?refresh=true",
            {"query": {"match_all": {}}}))
    ajoute("dbq, zero correspondance", simple,
           ("POST", f"/{INDEX}/_delete_by_query?refresh=true",
            {"query": {"term": {"tenant": "zzz"}}}))
    ajoute("dbq, une borne de date", simple,
           ("POST", f"/{INDEX}/_delete_by_query?refresh=true",
            {"query": {"range": {"paru": {"lt": "2026-04-01"}}}}))
    ajoute("dbq, un bool", simple,
           ("POST", f"/{INDEX}/_delete_by_query?refresh=true",
            {"query": {"bool": {"must": [{"match": {"txt": "document"}}],
                                "must_not": [{"term": {"tenant": "b"}}]}}}))
    ajoute("ubq, un terme", simple,
           ("POST", f"/{INDEX}/_update_by_query?refresh=true",
            {"query": {"term": {"tenant": "a"}}}))
    ajoute("ubq, sans corps du tout", simple,
           ("POST", f"/{INDEX}/_update_by_query?refresh=true", None))
    ajoute("ubq, deux fois de suite", simple,
           ("POST", f"/{INDEX}/_update_by_query?refresh=true", {"query": {"match_all": {}}}),
           ("POST", f"/{INDEX}/_update_by_query?refresh=true", {"query": {"match_all": {}}}))
    ajoute("ubq, zero correspondance", simple,
           ("POST", f"/{INDEX}/_update_by_query?refresh=true",
            {"query": {"term": {"tenant": "zzz"}}}))

    # -- max_docs et scroll_size : les compteurs, et **quels** documents ----
    for m in (1, 3, 5, 6, 99):
        ajoute(f"dbq, max_docs={m}", simple,
               ("POST", f"/{INDEX}/_delete_by_query?max_docs={m}&refresh=true",
                {"query": {"match_all": {}}}))
    ajoute("ubq, max_docs=2", simple,
           ("POST", f"/{INDEX}/_update_by_query?max_docs=2&refresh=true",
            {"query": {"match_all": {}}}))
    for taille in (1, 2, 3, 5, 6, 7, 1000):
        ajoute(f"dbq, scroll_size={taille}", simple,
               ("POST", f"/{INDEX}/_delete_by_query?scroll_size={taille}&refresh=true",
                {"query": {"match_all": {}}}))
    ajoute("dbq, max_docs=5 scroll_size=2", simple,
           ("POST", f"/{INDEX}/_delete_by_query?max_docs=5&scroll_size=2&refresh=true",
            {"query": {"match_all": {}}}))

    # -- les conflits : le coeur du sujet ----------------------------------
    #
    # Une ecriture **non rafraichie** est exactement ce qui les produit : la
    # recherche voit l'ancien document et son `_seq_no`, l'ecriture trouve le
    # nouveau. C'est le seul moyen de les provoquer sans course.
    def conflit_sur(doc_id):
        def prep(base):
            remplir(base)
            http(base, "POST", f"/{INDEX}/_doc/{doc_id}",
                 {"tenant": "a", "n": 99, "txt": "reecrit", "paru": "2026-01-15"})
        return prep

    ajoute("dbq, conflit sur le premier, abort", conflit_sur("0"),
           ("POST", f"/{INDEX}/_delete_by_query", {"query": {"match_all": {}}}))
    ajoute("dbq, conflit sur le premier, proceed", conflit_sur("0"),
           ("POST", f"/{INDEX}/_delete_by_query?conflicts=proceed",
            {"query": {"match_all": {}}}))
    ajoute("dbq, conflit lot 1/3, abort", conflit_sur("0"),
           ("POST", f"/{INDEX}/_delete_by_query?scroll_size=2",
            {"query": {"match_all": {}}}))
    ajoute("dbq, conflit lot 3/3, abort", conflit_sur("4"),
           ("POST", f"/{INDEX}/_delete_by_query?scroll_size=2",
            {"query": {"match_all": {}}}))
    ajoute("dbq, conflit lot 2/3, proceed", conflit_sur("2"),
           ("POST", f"/{INDEX}/_delete_by_query?scroll_size=2&conflicts=proceed",
            {"query": {"match_all": {}}}))
    ajoute("ubq, conflit sur le premier, abort", conflit_sur("0"),
           ("POST", f"/{INDEX}/_update_by_query", {"query": {"match_all": {}}}))
    ajoute("ubq, conflit sur le premier, proceed", conflit_sur("0"),
           ("POST", f"/{INDEX}/_update_by_query?conflicts=proceed",
            {"query": {"match_all": {}}}))

    # -- l'expression d'index ----------------------------------------------
    ajoute("dbq, liste d'index", deux,
           ("POST", f"/{INDEX},{INDEX_B}/_delete_by_query?refresh=true",
            {"query": {"term": {"tenant": "a"}}}))
    ajoute("dbq, joker", deux,
           ("POST", f"/{INDEX}*/_delete_by_query?refresh=true",
            {"query": {"term": {"tenant": "a"}}}))
    ajoute("dbq, joker et max_docs", deux,
           ("POST", f"/{INDEX}*/_delete_by_query?max_docs=3&refresh=true",
            {"query": {"match_all": {}}}))
    ajoute("dbq, joker et scroll_size", deux,
           ("POST", f"/{INDEX}*/_delete_by_query?scroll_size=3&refresh=true",
            {"query": {"match_all": {}}}))
    ajoute("ubq, liste d'index", deux,
           ("POST", f"/{INDEX},{INDEX_B}/_update_by_query?refresh=true",
            {"query": {"match_all": {}}}))
    ajoute("dbq, motif sans correspondance", simple,
           ("POST", "/rien-du-tout-*/_delete_by_query", {"query": {"match_all": {}}}))
    ajoute("dbq, index inconnu", simple,
           ("POST", "/rien-du-tout/_delete_by_query", {"query": {"match_all": {}}}))
    ajoute("dbq, index inconnu tolere", simple,
           ("POST", f"/{INDEX},rien-du-tout/_delete_by_query"
                    "?ignore_unavailable=true&refresh=true",
            {"query": {"term": {"tenant": "a"}}}))
    ajoute("dbq, sur un alias", lambda base: (
        remplir(base),
        http(base, "POST", "/_aliases",
             {"actions": [{"add": {"index": INDEX, "alias": "sonde-par-requete-alias"}}]})),
           ("POST", "/sonde-par-requete-alias/_delete_by_query?refresh=true",
            {"query": {"term": {"tenant": "a"}}}))

    # -- ce que les deux serveurs doivent refuser ---------------------------
    ajoute("dbq, sans query", simple,
           ("POST", f"/{INDEX}/_delete_by_query", {}))
    ajoute("dbq, sans corps du tout", simple,
           ("POST", f"/{INDEX}/_delete_by_query", None))
    ajoute("dbq, query nulle", simple,
           ("POST", f"/{INDEX}/_delete_by_query", {"query": None}))
    ajoute("dbq, clause inconnue", simple,
           ("POST", f"/{INDEX}/_delete_by_query", {"query": {"pas_une_clause": {}}}))
    ajoute("dbq, clause inconnue sans index vise", simple,
           ("POST", "/rien-du-tout-*/_delete_by_query", {"query": {"pas_une_clause": {}}}))
    ajoute("dbq, conflicts invalide", simple,
           ("POST", f"/{INDEX}/_delete_by_query?conflicts=zzz", {"query": {"match_all": {}}}))
    ajoute("dbq, max_docs=0", simple,
           ("POST", f"/{INDEX}/_delete_by_query?max_docs=0", {"query": {"match_all": {}}}))
    ajoute("dbq, max_docs illisible", simple,
           ("POST", f"/{INDEX}/_delete_by_query?max_docs=abc", {"query": {"match_all": {}}}))
    ajoute("dbq, scroll_size=0", simple,
           ("POST", f"/{INDEX}/_delete_by_query?scroll_size=0", {"query": {"match_all": {}}}))
    ajoute("dbq, scroll_size hors bornes", simple,
           ("POST", f"/{INDEX}/_delete_by_query?scroll_size=10001",
            {"query": {"match_all": {}}}))
    ajoute("dbq, refresh=wait_for", simple,
           ("POST", f"/{INDEX}/_delete_by_query?refresh=wait_for",
            {"query": {"match_all": {}}}))
    ajoute("dbq, parametre inconnu", simple,
           ("POST", f"/{INDEX}/_delete_by_query?pas_un_parametre=1",
            {"query": {"match_all": {}}}))
    ajoute("dbq, corps a clef inconnue", simple,
           ("POST", f"/{INDEX}/_delete_by_query",
            {"query": {"match_all": {}}, "pas_une_clef": 1}))
    ajoute("dbq, from dans le corps", simple,
           ("POST", f"/{INDEX}/_delete_by_query", {"query": {"match_all": {}}, "from": 1}))
    ajoute("dbq, en GET", simple,
           ("GET", f"/{INDEX}/_delete_by_query", {"query": {"match_all": {}}}))
    ajoute("ubq, en GET", simple,
           ("GET", f"/{INDEX}/_update_by_query", {"query": {"match_all": {}}}))
    ajoute("ubq, script Painless", simple,
           ("POST", f"/{INDEX}/_update_by_query",
            {"query": {"match_all": {}}, "script": {"source": "ctx._source.n++"}}))
    ajoute("ubq, script vide", simple,
           ("POST", f"/{INDEX}/_update_by_query",
            {"query": {"match_all": {}}, "script": {}}))
    ajoute("dbq, wait_for_completion=false", simple,
           ("POST", f"/{INDEX}/_delete_by_query?wait_for_completion=false",
            {"query": {"match_all": {}}}))
    ajoute("dbq, slices", simple,
           ("POST", f"/{INDEX}/_delete_by_query?slices=2", {"query": {"match_all": {}}}))
    ajoute("dbq, slice dans le corps", simple,
           ("POST", f"/{INDEX}/_delete_by_query",
            {"query": {"match_all": {}}, "slice": {"id": 0, "max": 2}}))
    ajoute("dbq, requests_per_second", simple,
           ("POST", f"/{INDEX}/_delete_by_query?requests_per_second=10",
            {"query": {"match_all": {}}}))
    ajoute("dbq, q en parametre", simple,
           ("POST", f"/{INDEX}/_delete_by_query?q=tenant:a", None))
    ajoute("dbq, terminate_after", simple,
           ("POST", f"/{INDEX}/_delete_by_query?terminate_after=2",
            {"query": {"match_all": {}}}))
    ajoute("ubq, pipeline", simple,
           ("POST", f"/{INDEX}/_update_by_query?pipeline=inexistant",
            {"query": {"match_all": {}}}))
    # `max_docs` et `conflicts` dans le **corps** : c'est la que le client
    # officiel les met, quoi qu'en dise la documentation.
    ajoute("dbq, max_docs dans le corps", simple,
           ("POST", f"/{INDEX}/_delete_by_query?refresh=true",
            {"query": {"match_all": {}}, "max_docs": 1}))
    ajoute("ubq, max_docs dans le corps", simple,
           ("POST", f"/{INDEX}/_update_by_query?refresh=true",
            {"query": {"match_all": {}}, "max_docs": 2}))
    ajoute("dbq, max_docs des deux cotes, d'accord", simple,
           ("POST", f"/{INDEX}/_delete_by_query?max_docs=2&refresh=true",
            {"query": {"match_all": {}}, "max_docs": 2}))
    ajoute("dbq, max_docs des deux cotes, en desaccord", simple,
           ("POST", f"/{INDEX}/_delete_by_query?max_docs=1&refresh=true",
            {"query": {"match_all": {}}, "max_docs": 3}))
    ajoute("ubq, conflicts dans le corps", conflit_sur("0"),
           ("POST", f"/{INDEX}/_update_by_query",
            {"query": {"match_all": {}}, "conflicts": "proceed"}))
    ajoute("ubq, conflicts, le parametre l'emporte", conflit_sur("0"),
           ("POST", f"/{INDEX}/_update_by_query?conflicts=abort",
            {"query": {"match_all": {}}, "conflicts": "proceed"}))
    ajoute("dbq, conflicts illisible dans le corps", simple,
           ("POST", f"/{INDEX}/_delete_by_query",
            {"query": {"match_all": {}}, "conflicts": "zzz"}))
    # Les valeurs par defaut d'ES, ecrites explicitement : elles ne demandent
    # rien, donc elles passent des deux cotes.
    ajoute("dbq, slices=1 (le defaut)", simple,
           ("POST", f"/{INDEX}/_delete_by_query?slices=1&refresh=true",
            {"query": {"match_all": {}}}))
    ajoute("dbq, requests_per_second=-1 (le defaut)", simple,
           ("POST", f"/{INDEX}/_delete_by_query?requests_per_second=-1&refresh=true",
            {"query": {"match_all": {}}}))
    ajoute("dbq, wait_for_completion=true (le defaut)", simple,
           ("POST", f"/{INDEX}/_delete_by_query?wait_for_completion=true&refresh=true",
            {"query": {"match_all": {}}}))
    return tous


# ---------------------------------------------------------------------------
# Les refus assumes : un par ecart laisse passer, avec sa raison
# ---------------------------------------------------------------------------

REFUS_ASSUMES = {
    "dbq, slices":
        "ES decoupe le travail en taches paralleles et ajoute une section "
        "[slices] a la reponse ; ferrite est mono-shard et synchrone. Le "
        "refuser est le seul choix qui ne mente pas sur ce qui a tourne",
    "dbq, slice dans le corps":
        "meme raison : une tranche n'a de sens que s'il y en a d'autres en "
        "parallele",
    "dbq, requests_per_second":
        "il regule le debit et remplit [throttled_millis] ; ferrite le rendrait "
        "a zero, c'est-a-dire une valeur plausible pour une regulation qui n'a "
        "pas eu lieu",
    "dbq, q en parametre":
        "la recherche par chaine (`query_string`) n'est pas implementee, et "
        "c'est un refus deja declare sur `_search`",
    "dbq, terminate_after":
        "il arrete la recherche a N documents par shard sans arreter "
        "l'ecriture : le [total] rendu ne dirait plus combien de documents la "
        "commande a traites",
    "ubq, pipeline":
        "les pipelines d'ingestion sont hors perimetre. ES, lui, accepte la "
        "requete et rend un echec **par document** — la meme information, mais "
        "apres avoir tout parcouru",
    "dbq, wait_for_completion=false":
        "il rend une **tache** que le client suit ensuite sur [_tasks] ; "
        "ferrite n'a pas d'API de taches, et rendre un identifiant bidon "
        "enverrait le client interroger une route qui n'existe pas",
    "dbq, corps a clef inconnue":
        "les deux refusent, avec deux types d'erreur : `parsing_exception` chez "
        "ES, le type propre a ferrite pour un refus de perimetre — c'est ce qui "
        "permet au rapport de conformance de compter un cout de perimetre "
        "plutot qu'une regression",
    "dbq, from dans le corps":
        "les deux refusent ; ES range le refus en "
        "`action_request_validation_exception`, ferrite en refus de perimetre",
    "ubq, script Painless":
        "Painless est hors perimetre, et c'est le seul refus de cette liste qui "
        "coute vraiment quelque chose : ES **accepte** et applique le script. "
        "Sans script, la route reindexe depuis le `_source`, ce qui couvre le "
        "geste d'apres un changement de mapping — mais pas « incremente un "
        "compteur sur mille documents »",
    "dbq, scroll_size hors bornes":
        "les deux refusent, avec le meme message au mot pres ; ES l'enveloppe "
        "dans un `search_phase_execution_exception` (« all shards failed »), "
        "ferrite rend directement la cause racine "
        "`illegal_argument_exception` — le type qu'ES lui-meme met dans son "
        "`root_cause`",
    "ubq, script vide":
        "les deux refusent ; ES parce que le script n'a ni [source] ni [id], "
        "ferrite parce que la clef [script] suffit a dire que Painless est "
        "demande",
}


def main():
    ferrite = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:9200"
    es = sys.argv[2] if len(sys.argv) > 2 else "http://localhost:9201"
    detail = "--detail" in sys.argv
    dispo = []
    for nom, base in (("ferrite", ferrite), ("es", es)):
        try:
            http(base, "GET", "/")
            dispo.append((nom, base))
        except Exception as exc:  # noqa: BLE001
            print(f"# {nom} indisponible ({base}) : {exc}")
    # Une sonde differentielle qui ne trouve qu'un serveur annonce « tout
    # identique » sans avoir rien compare : elle s'arrete plutot que de le faire.
    if len(dispo) < 2:
        raise SystemExit(
            f"# {len(dispo)} serveur(s) sur 2 : une comparaison a besoin des "
            f"deux, sinon son verdict ne veut rien dire.")

    total = ecarts = assumes = 0
    for libelle, prep, appels in cas():
        vus = []
        for _, base in dispo:
            prep(base)
            reponse = None
            for methode, chemin, corps in appels:
                reponse = http(base, methode, chemin, corps)
            vus.append((normalise(*reponse), etat(base), etat(base, INDEX_B)))
        total += 1
        differe = vus[0] != vus[1]
        assume = libelle in REFUS_ASSUMES
        marque = "~" if differe and assume else ("*" if differe else " ")
        print(f"{marque} {libelle:44} {json.dumps(vus[0][0][1], default=str)[:90]}")
        if differe and assume:
            assumes += 1
            print(f"      refus assume : {REFUS_ASSUMES[libelle]}")
        elif differe:
            ecarts += 1
            for (nom, _), vu in zip(dispo, vus):
                print(f"      {nom:8} reponse {json.dumps(vu[0], default=str)[:400]}")
            for (nom, _), vu in zip(dispo, vus):
                print(f"      {nom:8} etat    {json.dumps(vu[1], default=str)[:400]}")
                if vu[2] and "index illisible" not in vu[2]:
                    print(f"      {nom:8} etat b  {json.dumps(vu[2], default=str)[:300]}")
        elif detail:
            print(f"      {json.dumps(vus[0][1], default=str)[:300]}")

    for _, base in dispo:
        for index in (INDEX, INDEX_B):
            http(base, "DELETE", f"/{index}")
    print(f"\n{total - ecarts - assumes}/{total} identiques, "
          f"{assumes} refus assume(s), {ecarts} ecart(s)")
    return 1 if ecarts else 0


if __name__ == "__main__":
    sys.exit(main())
