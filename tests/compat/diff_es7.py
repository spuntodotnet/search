#!/usr/bin/env python3
"""ferrite peut-il remplacer une instance Elasticsearch **7.10.2** existante ?

`probe_es7.py` repond a la question du *client* : le code 7.x se connecte-t-il.
Celui-ci repond a la question de l'*instance* : peut-on reprendre ses index,
ses documents et ses requetes.

    python3 tests/compat/diff_es7.py [ferrite_url] [es7_url] [--sans-ecriture]

Trois phases, dans l'ordre ou une migration se heurte aux problemes :

  1. Le schema  — chaque index de l'instance 7.x est exporte (`GET /{index}`)
                  et rejoue sur ferrite. Ce qu'il faut retirer pour que ca
                  passe est la liste des incompatibilites de mapping.
  2. Les documents — un echantillon est transfere par `scan` + `bulk`. Les
                  refus sont classes par cause : c'est la ou les documents
                  imbriques se manifestent.
  3. Les requetes — meme corpus des deux cotes, la batterie de
                  `diff_relevance.py`, et pour chaque requete : meme total,
                  memes documents, meme ordre.

Les phases 1 et 2 ne font que **lire** l'instance 7.x : elles n'ecrivent que
sur ferrite. La phase 3 a besoin d'indexer le corpus des deux cotes ;
`--sans-ecriture` la desactive, ce qui rend le script sur a pointer vers une
instance qui compte.

Le client officiel 7.10.1 pilote les deux serveurs (le client 8.x, lui, refuse
de parler a un serveur 7.10) :

    pip install "elasticsearch==7.10.1"

Outil de developpement : exige un Elasticsearch 7.x lance a cote.
"""
import sys

from elasticsearch import Elasticsearch, helpers

import corpus
from diff_relevance import requetes

FERRITE = next((a for a in sys.argv[1:] if not a.startswith("-")), "http://localhost:9200")
ES = next((a for a in sys.argv[1:] if not a.startswith("-")
           and a != FERRITE), "http://localhost:9201")
SANS_ECRITURE = "--sans-ecriture" in sys.argv
# `--inventaire` ne demande qu'une URL : celle de l'instance a inventorier.
INVENTAIRE = "--inventaire" in sys.argv
if INVENTAIRE and len(sys.argv) == 2:
    FERRITE, ES = None, "http://localhost:9200"
elif INVENTAIRE:
    ES = FERRITE

INDEX_CORPUS = "compat_es7"
PREFIXE_MIGRE = "migre_"
ECHANTILLON = 200
TAILLE = 25

# Un index tel qu'une application de l'ere 7.x en declare un : analyzer de
# langue, analyzer sur mesure, sous-objet, date formatee, multi-field. Il n'est
# cree que si l'instance visee n'a aucun index applicatif — pointer le script
# vers une vraie instance doit rapporter l'etat de *ses* index, pas de celui-ci.
LEGACY_INDEX = "legacy_7x"
LEGACY_DEF = {
    "settings": {
        "number_of_shards": 1,
        "number_of_replicas": 0,
        "refresh_interval": "1s",
        "analysis": {
            "analyzer": {
                "fr_produit": {
                    "type": "custom",
                    "tokenizer": "standard",
                    "filter": ["lowercase", "asciifolding"],
                }
            }
        },
    },
    "mappings": {
        "properties": {
            "titre": {"type": "text", "analyzer": "fr_produit",
                      "fields": {"keyword": {"type": "keyword", "ignore_above": 256}}},
            "description": {"type": "text", "analyzer": "french"},
            "reference": {"type": "keyword"},
            "prix": {"type": "double"},
            "stock": {"type": "integer"},
            "cree_le": {"type": "date", "format": "yyyy-MM-dd HH:mm:ss"},
            "fournisseur": {"properties": {"nom": {"type": "keyword"},
                                           "pays": {"type": "keyword"}}},
            "etiquettes": {"type": "keyword"},
        }
    },
}
LEGACY_DOCS = [
    {"titre": "Clavier compact", "description": "un clavier mecanique silencieux",
     "reference": "KB-1", "prix": 89.9, "stock": 12,
     "cree_le": "2021-03-04 10:00:00",
     "fournisseur": {"nom": "Atelier", "pays": "FR"}, "etiquettes": ["bureau"]},
    {"titre": "Ecran portable", "description": "un ecran leger pour la mobilite",
     "reference": "SC-2", "prix": 219.0, "stock": 3,
     "cree_le": "2021-06-11 09:30:00",
     "fournisseur": {"nom": "Optique", "pays": "DE"}, "etiquettes": ["mobilite", "video"]},
]

# Reglages qu'Elasticsearch lui-meme refuse a la creation : ils decrivent
# l'index existant, ils ne le configurent pas. Les retirer fait partie de
# n'importe quelle migration, y compris d'un ES 7 vers un ES 8.
REGLAGES_PRIVES = ("uuid", "creation_date", "provided_name", "version",
                   "resize", "routing", "history", "verified_before_close")


def titre(texte):
    print(f"\n== {texte}")


def raison(exc):
    info = getattr(exc, "info", None)
    if isinstance(info, dict):
        err = info.get("error")
        if isinstance(err, dict):
            return (err.get("reason") or err.get("type") or "")[:220]
        if isinstance(err, str):
            return err[:220]
    return str(exc)[:220]


# ---------------------------------------------------------------------------
# Inventaire : ce que l'instance utilise, sans juger
# ---------------------------------------------------------------------------


def parcours_mapping(props, prefixe=""):
    """Rend (chemin, declaration) pour chaque champ, sous-objets compris."""
    for nom, decl in sorted((props or {}).items()):
        chemin = prefixe + nom
        yield chemin, decl
        yield from parcours_mapping(decl.get("properties"), chemin + ".")
        for sub, sub_decl in sorted((decl.get("fields") or {}).items()):
            yield f"{chemin}.{sub}", sub_decl


def inventaire(es):
    """Quels types de champ l'instance utilise-t-elle, et combien de fois ?

    Purement descriptif : le verdict, lui, se prouve en rejouant le schema sur
    ferrite (phase 1). Ne demande qu'une URL, ne fait que lire — c'est la
    commande a passer sur une instance qu'on n'a pas le droit de toucher.
    """
    titre(f"Inventaire des mappings de {ES}")
    types, structures = {}, {}
    exemples = {}
    for index in indices_applicatifs(es):
        props = es.indices.get_mapping(index=index)[index]["mappings"].get("properties", {})
        for chemin, decl in parcours_mapping(props):
            if "properties" in decl and decl.get("type") != "nested":
                cle = "object (sous-objet)"
            elif decl.get("type") == "nested":
                cle = "nested"
            elif decl.get("type") == "join":
                cle = "join (parent/enfant)"
            else:
                cle = decl.get("type", "?")
            cible = structures if " " in cle or cle == "nested" else types
            cible[cle] = cible.get(cle, 0) + 1
            exemples.setdefault(cle, f"{index}:{chemin}")

    if not types and not structures:
        print("  aucun index applicatif")
        return
    for titre_bloc, table in (("structures", structures), ("champs simples", types)):
        if not table:
            continue
        print(f"  {titre_bloc} :")
        for cle, n in sorted(table.items(), key=lambda kv: -kv[1]):
            print(f"    {n:>5}  {cle:<22} ex. {exemples[cle]}")
    print("\n  Les trois structures ne coutent pas la meme chose a reprendre :")
    print("    object  ES l'aplatit lui-meme en chemins pointes (`client.ville`)")
    print("    nested  chaque sous-objet est un document cache + une jointure de bloc")
    print("    join    des documents distincts reunis par une jointure a la requete")


def aplatis(settings):
    """`{"index": {"a": 1}}` -> `{"index.a": 1}`, comme les affiche ES."""
    plat = {}

    def parcours(prefixe, valeur):
        if isinstance(valeur, dict):
            for k, v in valeur.items():
                parcours(f"{prefixe}{k}.", v)
        else:
            plat[prefixe[:-1]] = valeur

    parcours("", settings)
    return plat


# ---------------------------------------------------------------------------
# Phase 1 : le schema
# ---------------------------------------------------------------------------


def indices_applicatifs(es):
    noms = sorted(n for n in es.indices.get_alias(index="*") if not n.startswith("."))
    return [n for n in noms if not n.startswith(PREFIXE_MIGRE) and n != INDEX_CORPUS]


def cree(ferrite, cible, corps):
    ferrite.indices.delete(index=cible, ignore=[404])
    ferrite.indices.create(index=cible, body=corps)


def nettoie_les_champs(ferrite, cible, props):
    """Le dernier recours : garder les champs que ferrite accepte.

    Chaque champ est essaye tel quel, puis reduit a son seul `type`, puis
    abandonne. C'est la degradation qu'une migration reelle assumerait, et sa
    liste chiffre ce que l'index perdrait.
    """
    gardes, degrades, abandonnes = {}, [], []
    for nom, champ in sorted(props.items()):
        essais = [champ]
        if champ.get("type"):
            essais.append({"type": champ["type"]})
        pourquoi = "champ objet/imbrique" if "properties" in champ else "?"
        for i, essai in enumerate(essais):
            try:
                cree(ferrite, cible, {"mappings": {"properties": {nom: essai}}})
            except Exception as exc:  # noqa: BLE001
                pourquoi = raison(exc)
                continue
            gardes[nom] = essai
            if i:
                degrades.append((nom, sorted(set(champ) - set(essai)), pourquoi))
            break
        else:
            abandonnes.append((nom, pourquoi))
    if not gardes:
        return None, degrades, abandonnes
    cree(ferrite, cible, {"mappings": {"properties": gardes}})
    return gardes, degrades, abandonnes


def rejoue_le_schema(ferrite, definition, cible):
    """Rejoue une definition d'index 7.x sur ferrite, en pelant les couches.

    Rend (etape_qui_passe, [ce qu'il a fallu retirer], [refus rencontres]).
    """
    settings = definition.get("settings", {}).get("index", {})
    mappings = definition.get("mappings", {})

    plat = aplatis({"index": settings})
    prive = {k: v for k, v in plat.items()
             if k.split(".")[1] in REGLAGES_PRIVES}
    public = {k: v for k, v in plat.items() if k not in prive}

    essais = [
        ("tel quel", {"settings": plat, "mappings": mappings}, []),
        ("sans les reglages prives",
         {"settings": public, "mappings": mappings},
         [f"settings {k}" for k in sorted(prive)]),
        ("mappings seuls", {"mappings": mappings},
         [f"settings {k}" for k in sorted(plat)]),
    ]

    refus = []
    for etape, corps, abandonne in essais:
        try:
            cree(ferrite, cible, corps)
            return etape, abandonne, refus
        except Exception as exc:  # noqa: BLE001
            refus.append((etape, raison(exc)))

    # Les mappings seuls ne passent pas : champ par champ, on garde ce qui
    # passe et on chiffre ce que l'index perd.
    props = mappings.get("properties", {})
    gardes, degrades, abandonnes = nettoie_les_champs(ferrite, cible, props)
    perdu = [f"champ [{nom}] : {pourquoi}" for nom, pourquoi in abandonnes]
    perdu += [f"champ [{nom}] : parametres retires {params} ({pourquoi})"
              for nom, params, pourquoi in degrades]
    if gardes is None:
        ferrite.indices.delete(index=cible, ignore=[404])
        return None, perdu, refus
    return (f"mappings nettoyes ({len(gardes)}/{len(props)} champs)", perdu, refus)


def phase_schema(ferrite, es, noms):
    titre("Phase 1 — le schema des index, exporte de l'instance 7.x et rejoue")
    rapport = {}
    for nom in noms:
        definition = es.indices.get(index=nom)[nom]
        cible = PREFIXE_MIGRE + nom
        etape, detail, refus = rejoue_le_schema(ferrite, definition, cible)
        rapport[nom] = (etape, cible)
        if etape:
            print(f"  [ok] {nom} -> cree sur ferrite ({etape})")
            for quoi in detail:
                print(f"       retire : {quoi}")
            for etape_ko, pourquoi in refus:
                print(f"       ({etape_ko} refuse : {pourquoi})")
        else:
            print(f"  [KO] {nom} -> ferrite ne peut pas heberger cet index")
            for etape_ko, pourquoi in refus:
                print(f"       {etape_ko} : {pourquoi}")
            for champ, pourquoi in detail:
                print(f"       champ [{champ}] : {pourquoi}")
    return rapport


# ---------------------------------------------------------------------------
# Phase 2 : les documents
# ---------------------------------------------------------------------------


def phase_documents(ferrite, es, rapport):
    titre(f"Phase 2 — transfert des documents (scan 7.x -> bulk ferrite, "
          f"{ECHANTILLON} max par index)")
    for source, (etape, cible) in rapport.items():
        if not etape:
            print(f"  [--] {source} : pas d'index cible, transfert impossible")
            continue
        actions = []
        for doc in helpers.scan(es, index=source, size=100, query={"query": {"match_all": {}}}):
            actions.append({"_index": cible, "_id": doc["_id"], "_source": doc["_source"]})
            if len(actions) >= ECHANTILLON:
                break
        if not actions:
            print(f"  [--] {source} : aucun document")
            continue
        ok, erreurs = helpers.bulk(ferrite, actions, raise_on_error=False,
                                   stats_only=False, refresh=True)
        causes = {}
        for e in erreurs:
            item = next(iter(e.values()))
            r = (item.get("error") or {}).get("reason", "?")
            causes[r] = causes.get(r, 0) + 1
        etat = "ok" if not erreurs else "KO"
        print(f"  [{etat}] {source} : {ok}/{len(actions)} documents transferes")
        for r, n in sorted(causes.items(), key=lambda kv: -kv[1])[:5]:
            print(f"       {n} refus : {r[:200]}")


# ---------------------------------------------------------------------------
# Phase 3 : les requetes
# ---------------------------------------------------------------------------


def indexe_le_corpus(client, docs):
    client.indices.delete(index=INDEX_CORPUS, ignore=[404])
    client.indices.create(index=INDEX_CORPUS, body={
        "settings": {"number_of_shards": 1, "number_of_replicas": 0},
        "mappings": corpus.MAPPINGS})
    ops = []
    for doc_id, doc in docs:
        ops.append({"index": {"_index": INDEX_CORPUS, "_id": doc_id}})
        ops.append(doc)
    for debut in range(0, len(ops), 400):
        client.bulk(body=ops[debut:debut + 400])
    client.indices.refresh(index=INDEX_CORPUS)


def phase_requetes(ferrite, es):
    docs = corpus.documents()
    titre(f"Phase 3 — {len(docs)} documents identiques des deux cotes, "
          f"la batterie de diff_relevance.py")
    indexe_le_corpus(ferrite, docs)
    indexe_le_corpus(es, docs)

    qs = requetes(docs)
    total = identiques = ex_aequo = 0
    ecarts = []
    refuses = []
    for label, query, sort in qs:
        corps = {"query": query, "size": TAILLE}
        if sort:
            corps["sort"] = sort
        total += 1
        try:
            rf = ferrite.search(index=INDEX_CORPUS, body=corps)
        except Exception as exc:  # noqa: BLE001
            refuses.append((label, raison(exc)))
            continue
        re_ = es.search(index=INDEX_CORPUS, body=corps)

        tf, te = rf["hits"]["total"]["value"], re_["hits"]["total"]["value"]
        if tf != te:
            ecarts.append((label, f"total ferrite={tf} / ES 7={te}"))
            continue
        idf = [h["_id"] for h in rf["hits"]["hits"]]
        ide = [h["_id"] for h in re_["hits"]["hits"]]
        if idf == ide:
            identiques += 1
            continue
        if set(idf) != set(ide):
            ecarts.append((label, f"documents differents (total {tf}) — manquants : "
                                  f"{sorted(set(ide) - set(idf))[:5]}"))
            continue
        scores = {h["_id"]: h["_score"] for h in re_["hits"]["hits"]}
        divergent = [(a, b) for a, b in zip(idf, ide)
                     if a != b and scores.get(a) != scores.get(b)]
        if divergent:
            a, b = divergent[0]
            ecarts.append((label, f"ordre different — ferrite place [{a}] la ou "
                                  f"ES 7 place [{b}]"))
        else:
            ex_aequo += 1

    for label, detail in refuses:
        print(f"  [refus ferrite] {label}\n                  {detail}")
    for label, detail in ecarts:
        print(f"  [ecart] {label}\n          {detail}")
    print(f"\n  {identiques}/{total} requetes : memes documents, meme ordre qu'ES 7.10.2")
    if ex_aequo:
        print(f"  {ex_aequo}/{total} : ordre permute uniquement entre ex aequo d'ES 7")
    print(f"  {len(refuses)}/{total} refusees par ferrite, {len(ecarts)}/{total} ecarts reels")

    for client in (ferrite, es):
        client.indices.delete(index=INDEX_CORPUS, ignore=[404])
    return len(ecarts)


# ---------------------------------------------------------------------------
# Phase 4 : la forme des reponses, la ou un code 7.x la lit
# ---------------------------------------------------------------------------

LECTURES = [
    ("reponse d'indexation", lambda c, i: c.index(index=i, id="zz", body={"titre": "x"}, refresh=True)),
    ("reponse de get", lambda c, i: c.get(index=i, id="zz")),
    ("hit de recherche", lambda c, i: c.search(index=i, body={"query": {"match_all": {}}, "size": 1})["hits"]["hits"][0]),
    ("enveloppe hits", lambda c, i: {k: v for k, v in c.search(index=i, body={"query": {"match_all": {}}, "size": 0}).items() if k != "took"}),
    ("item de bulk", lambda c, i: c.bulk(body=[{"index": {"_index": i, "_id": "yy"}}, {"titre": "y"}], refresh=True)["items"][0]),
    ("reponse de delete", lambda c, i: c.delete(index=i, id="yy", refresh=True)),
    ("_cluster/health", lambda c, i: c.cluster.health()),
]


def cles(valeur, prefixe=""):
    if isinstance(valeur, dict):
        out = set()
        for k, v in valeur.items():
            out |= {prefixe + k} | cles(v, prefixe + k + ".")
        return out
    return set()


def phase_forme(ferrite, es):
    titre("Phase 4 — la forme des reponses qu'un code 7.x lit")
    for client in (ferrite, es):
        client.indices.delete(index="forme7", ignore=[404])
        client.indices.create(index="forme7", body={"mappings": {"properties": {"titre": {"type": "text"}}}})
    for nom, appel in LECTURES:
        try:
            cf, ce = cles(appel(ferrite, "forme7")), cles(appel(es, "forme7"))
        except Exception as exc:  # noqa: BLE001
            print(f"  [KO] {nom} : {raison(exc)}")
            continue
        manque = sorted(k for k in ce - cf if "." not in k or k.split(".")[0] in cf)
        trop = sorted(k for k in cf - ce if "." not in k or k.split(".")[0] in ce)
        if not manque and not trop:
            print(f"  [ok] {nom} : memes cles qu'ES 7.10.2")
        else:
            print(f"  [!!] {nom} :")
            if manque:
                print(f"       absentes chez ferrite : {manque}")
            if trop:
                print(f"       en plus chez ferrite  : {trop}")
    for client in (ferrite, es):
        client.indices.delete(index="forme7", ignore=[404])


def main():
    es = Elasticsearch(ES, timeout=60)
    if INVENTAIRE:
        # Mode lecture pure : ferrite n'a meme pas besoin de tourner.
        inventaire(es)
        return 0

    ferrite = Elasticsearch(FERRITE, timeout=60)
    v = es.info()["version"]["number"]
    print(f"== ferrite {FERRITE} — Elasticsearch {v} {ES}")
    if not v.startswith("7."):
        print("   (ce script compare a un serveur 7.x ; "
              "diff_against_es.py fait le meme travail contre un 8.x)")

    noms = indices_applicatifs(es)
    if not noms and not SANS_ECRITURE:
        print(f"   aucun index applicatif sur l'instance : creation de "
              f"[{LEGACY_INDEX}], un index typique de l'ere 7.x")
        es.indices.create(index=LEGACY_INDEX, body=LEGACY_DEF)
        for n, doc in enumerate(LEGACY_DOCS, 1):
            es.index(index=LEGACY_INDEX, id=str(n), body=doc, refresh=True)
        noms = [LEGACY_INDEX]

    rapport = phase_schema(ferrite, es, noms)
    phase_documents(ferrite, es, rapport)
    for _, cible in rapport.values():
        ferrite.indices.delete(index=cible, ignore=[404])

    if SANS_ECRITURE:
        print("\n== phases 3 et 4 sautees (--sans-ecriture)")
        return 0
    ecarts = phase_requetes(ferrite, es)
    phase_forme(ferrite, es)
    return 1 if ecarts else 0


if __name__ == "__main__":
    sys.exit(main())
