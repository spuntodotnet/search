#!/usr/bin/env python3
"""Un index, plusieurs index, un alias : ferrite repond-il comme Elasticsearch ?

Deux usages reels tiennent ce fichier, tous deux constates sur du code de
production qui refusait de se brancher sur ferrite :

- **la recherche multi-index par tableau** — `es.search(index=["a", "b"])`, ce
  qu'ecrit un service qui cherche dans plusieurs catalogues d'un coup ;
- **l'alias sur des index quotidiens** — `audits` qui designe
  `audits-2026.08.01`, `…02`, `…03`, plus une purge en `DELETE /audits-2026.07.*`.

Le meme corpus est **reparti a l'identique** des deux cotes (repartition
deterministe par identifiant), puis les memes appels sont envoyes aux deux
serveurs. La comparaison porte sur ce qui casse un client : le nombre total, la
liste ordonnee des `(_index, _id)`, le compte de shards, les agregations champ
par champ, et — pour ce qui doit echouer — le statut HTTP et le type d'erreur.

Les scores ne sont pas compares : chaque index calcule les siens sur ses
propres statistiques (c'est aussi ce que fait ES entre shards), donc c'est
**l'ordre** qui a un sens, pas la valeur.

    python3 tests/compat/diff_multi_index.py [ferrite_url] [es_url]
    python3 tests/compat/diff_multi_index.py --calibrer [es_a] [es_b]

`--calibrer` fait tourner la batterie contre **deux** Elasticsearch : si elle
n'est pas verte la, ses verdicts sur ferrite ne veulent rien dire. La batterie
modifie l'etat du serveur (elle pose des alias, elle supprime des index) : la
calibrer contre un seul serveur rejouerait chaque appel deux fois et
mesurerait la difference entre « avant » et « apres », pas entre deux moteurs.

Outil de developpement : exige un Elasticsearch 8.x lance a cote (Docker).
"""
import json
import sys

from elasticsearch import ApiError, Elasticsearch

import corpus

PREFIXE = "compat_mi"
# Trois index « metier », comme les catalogues d'un service qui cherche dans
# plusieurs a la fois.
CATALOGUES = [f"{PREFIXE}_audio", f"{PREFIXE}_video", f"{PREFIXE}_reseau"]
# Trois index quotidiens plus un ancien, comme une retention par jour.
JOURS = [f"{PREFIXE}-2026.08.0{i}" for i in (1, 2, 3)]
VIEUX = f"{PREFIXE}-2026.07.31"
ALIAS = "compat_mi_tout"
# Un index dont le mapping porte un champ que les autres n'ont pas : c'est le
# cas d'une famille d'index quotidiens ou seul le plus recent connait un champ
# ajoute la semaine derniere.
DIVERGENT = f"{PREFIXE}_recent"
TOUS = CATALOGUES + JOURS + [VIEUX]


def repartir(docs):
    """`index -> [(id, doc)]`, de facon deterministe.

    La repartition doit etre identique des deux cotes : sinon les scores, et
    donc l'ordre, differeraient pour une raison qui n'a rien a voir avec le
    moteur.
    """
    par_index = {nom: [] for nom in TOUS}
    for i, (doc_id, doc) in enumerate(docs):
        par_index[TOUS[i % len(TOUS)]].append((doc_id, doc))
    return par_index


def preparer(client, docs):
    for nom in TOUS + [DIVERGENT]:
        client.options(ignore_status=404).indices.delete(index=nom)
    client.options(ignore_status=404).indices.delete_alias(index="*", name=ALIAS)
    for nom, contenu in repartir(docs).items():
        client.indices.create(
            index=nom,
            mappings=corpus.MAPPINGS,
            settings={"number_of_shards": 1, "number_of_replicas": 0},
        )
        ops = []
        for doc_id, doc in contenu:
            ops.append({"index": {"_index": nom, "_id": doc_id}})
            ops.append(doc)
        for debut in range(0, len(ops), 400):
            client.bulk(operations=ops[debut : debut + 400])
    mapping_etendu = {
        "properties": dict(corpus.MAPPINGS["properties"], canal={"type": "keyword"})
    }
    client.indices.create(
        index=DIVERGENT,
        mappings=mapping_etendu,
        settings={"number_of_shards": 1, "number_of_replicas": 0},
    )
    client.index(index=DIVERGENT, id="recent-1", document={
        "titre": "appareil recent", "corps": "appareil de la derniere fournee",
        "marque": "Sony", "categorie": "audio", "prix": 199.0, "stock": 3,
        "actif": True, "cree_le": "2026-07-01T00:00:00Z", "canal": "boutique",
    })
    client.indices.refresh(index=f"{PREFIXE}*")


def nettoyer(client):
    # Un `DELETE` en motif est refuse par defaut (`action.destructive_requires_name`) :
    # le menage nomme donc chaque index. Un nettoyage qui echoue fait cascader
    # tout le reste en « index already exists » au passage suivant.
    restants = [nom for nom in client.indices.get(index=f"{PREFIXE}*", ignore_unavailable=True)]
    for nom in restants:
        client.options(ignore_status=404).indices.delete(index=nom)
    client.cluster.put_settings(persistent={"action.destructive_requires_name": None})


# ---------------------------------------------------------------------------
# La batterie
# ---------------------------------------------------------------------------


def recherches():
    """`(libelle, kwargs de client.search)`."""
    q_texte = {"match": {"corps": "appareil"}}
    return [
        # --- le cas service-catalogs : un tableau d'index
        ("tableau de 2 index", {"index": CATALOGUES[:2], "query": q_texte}),
        ("tableau de 3 index", {"index": CATALOGUES, "query": q_texte}),
        ("tableau, match_all", {"index": CATALOGUES, "query": {"match_all": {}}}),
        ("liste en chaine", {"index": ",".join(CATALOGUES), "query": q_texte}),
        ("un seul index (temoin)", {"index": CATALOGUES[0], "query": q_texte}),
        # --- motifs
        ("motif large", {"index": f"{PREFIXE}*", "query": q_texte}),
        ("motif sur les jours", {"index": f"{PREFIXE}-2026.08.*", "query": q_texte}),
        ("_all", {"index": "_all", "query": q_texte}),
        ("*", {"index": "*", "query": q_texte}),
        (
            "motif moins exclusion",
            {"index": f"{PREFIXE}-2026.08.*,-{PREFIXE}-2026.08.02", "query": q_texte},
        ),
        ("motif sans correspondance", {"index": "fantome_*", "query": q_texte}),
        # --- alias
        ("alias sur 3 index", {"index": ALIAS, "query": q_texte}),
        ("alias + index", {"index": [ALIAS, CATALOGUES[0]], "query": q_texte}),
        ("alias, match_all", {"index": ALIAS, "query": {"match_all": {}}}),
        # --- pagination et tri, la ou la fusion peut se tromper
        ("pagination from=0", {"index": CATALOGUES, "query": q_texte, "from_": 0, "size": 5}),
        ("pagination from=5", {"index": CATALOGUES, "query": q_texte, "from_": 5, "size": 5}),
        ("pagination from=20", {"index": CATALOGUES, "query": q_texte, "from_": 20, "size": 10}),
        ("size=0", {"index": CATALOGUES, "query": q_texte, "size": 0}),
        ("size=100", {"index": CATALOGUES, "query": q_texte, "size": 100}),
        (
            "tri keyword asc",
            {"index": CATALOGUES, "query": q_texte, "sort": [{"marque": "asc"}], "size": 20},
        ),
        (
            "tri date desc",
            {"index": ALIAS, "query": {"match_all": {}}, "sort": [{"cree_le": "desc"}], "size": 15},
        ),
        (
            "tri sur deux cles",
            {
                "index": CATALOGUES,
                "query": {"match_all": {}},
                "sort": [{"categorie": "asc"}, {"prix": "desc"}],
                "size": 25,
            },
        ),
        (
            "tri + pagination",
            {
                "index": CATALOGUES,
                "query": {"match_all": {}},
                "sort": [{"stock": "asc"}],
                "from_": 10,
                "size": 10,
            },
        ),
        # --- filtres, pour verifier que la requete est bien rejouee par index
        (
            "bool filter multi-index",
            {
                "index": CATALOGUES,
                "query": {
                    "bool": {
                        "must": [q_texte],
                        "filter": [{"range": {"prix": {"gte": 200}}}],
                    }
                },
                "size": 20,
            },
        ),
        (
            "term sur keyword",
            {"index": ALIAS, "query": {"term": {"marque": "Sony"}}, "size": 20},
        ),
        # --- agregations : la fusion entre index est ici le vrai risque
        ("aggs terms multi-index", {"index": CATALOGUES, "aggs": {"f": {"terms": {"field": "marque"}}}, "size": 0}),
        ("aggs terms sur alias", {"index": ALIAS, "aggs": {"f": {"terms": {"field": "categorie"}}}, "size": 0}),
        ("aggs avg multi-index", {"index": CATALOGUES, "aggs": {"m": {"avg": {"field": "prix"}}}, "size": 0}),
        ("aggs stats multi-index", {"index": ALIAS, "aggs": {"m": {"stats": {"field": "prix"}}}, "size": 0}),
        ("aggs min/max dates", {"index": ALIAS, "aggs": {"a": {"min": {"field": "cree_le"}}, "b": {"max": {"field": "cree_le"}}}, "size": 0}),
        ("aggs value_count", {"index": TOUS, "aggs": {"m": {"value_count": {"field": "stock"}}}, "size": 0}),
        (
            "aggs terms + avg",
            {
                "index": ALIAS,
                "aggs": {"f": {"terms": {"field": "marque"}, "aggs": {"p": {"avg": {"field": "prix"}}}}},
                "size": 0,
            },
        ),
        (
            "aggs date_histogram",
            {
                "index": ALIAS,
                "aggs": {"d": {"date_histogram": {"field": "cree_le", "fixed_interval": "30d"}}},
                "size": 0,
            },
        ),
        (
            "aggs histogram + sous-terms",
            {
                "index": CATALOGUES,
                "aggs": {
                    "h": {
                        "histogram": {"field": "prix", "interval": 300},
                        "aggs": {"g": {"terms": {"field": "categorie", "size": 3}}},
                    }
                },
                "size": 0,
            },
        ),
        (
            "aggs sous requete",
            {"index": ALIAS, "query": q_texte, "aggs": {"f": {"terms": {"field": "marque", "size": 5}}}, "size": 0},
        ),
        ("aggs range multi-index", {"index": CATALOGUES, "aggs": {"r": {"range": {"field": "prix", "ranges": [{"to": 100}, {"from": 100, "to": 500}, {"from": 500}]}}}, "size": 0}),
        # --- ce qui doit echouer, et de la meme facon
        ("index absent", {"index": "fantome_absolu", "query": q_texte}),
        ("nom d'index reserve", {"index": "_route_reservee", "query": q_texte}),
        # Un champ que seul un des index vises connait : ES cherche quand meme,
        # les autres n'ayant simplement rien a rendre.
        (
            "champ connu d'un seul index",
            {"index": [DIVERGENT, CATALOGUES[0]], "query": {"term": {"canal": "boutique"}}},
        ),
        (
            "champ d'un seul index, dans un bool",
            {
                "index": [DIVERGENT] + CATALOGUES,
                "query": {"bool": {"should": [{"term": {"canal": "boutique"}}, q_texte]}},
                "size": 5,
            },
        ),
        (
            "tri sur un champ connu d'un seul index",
            {"index": [DIVERGENT, CATALOGUES[0]], "query": {"match_all": {}},
             "sort": [{"canal": "asc"}], "size": 5},
        ),
        (
            "agregation sur un champ connu d'un seul index",
            {"index": [DIVERGENT] + CATALOGUES, "size": 0,
             "aggs": {"f": {"terms": {"field": "canal"}}}},
        ),
        ("index absent dans une liste", {"index": [CATALOGUES[0], "fantome_absolu"], "query": q_texte}),
        ("champ inconnu partout", {"index": CATALOGUES, "query": {"term": {"champ_qui_nexiste_pas": "x"}}}),
    ]


def apres_coup():
    """Appels HTTP bruts : administration des alias, motifs, suppression.

    Bruts parce que ce qu'on compare ici, c'est la **forme** de la reponse et le
    statut, pas un objet du client.
    """
    return [
        # --- lecture des alias
        ("GET /_alias", "GET", "/_alias", None),
        ("GET /{index}/_alias", "GET", f"/{JOURS[0]}/_alias", None),
        ("GET /_alias/{nom}", "GET", f"/_alias/{ALIAS}", None),
        ("GET /_alias/{motif}", "GET", f"/_alias/{PREFIXE}*", None),
        ("GET /_alias/{absent}", "GET", "/_alias/fantome", None),
        ("HEAD /_alias/{nom}", "HEAD", f"/_alias/{ALIAS}", None),
        ("HEAD /_alias/{absent}", "HEAD", "/_alias/fantome", None),
        # --- pose et retrait
        ("PUT alias sur un index", "PUT", f"/{VIEUX}/_alias/mi_essai", None),
        ("GET apres pose", "GET", "/_alias/mi_essai", None),
        ("DELETE alias", "DELETE", f"/{VIEUX}/_alias/mi_essai", None),
        ("GET apres retrait", "GET", "/_alias/mi_essai", None),
        # --- le lot atomique
        (
            "POST /_aliases add",
            "POST",
            "/_aliases",
            {"actions": [{"add": {"index": JOURS[0], "alias": "mi_lot"}}]},
        ),
        (
            "POST /_aliases bascule",
            "POST",
            "/_aliases",
            {
                "actions": [
                    {"remove": {"index": JOURS[0], "alias": "mi_lot"}},
                    {"add": {"index": JOURS[1], "alias": "mi_lot"}},
                ]
            },
        ),
        ("GET apres bascule", "GET", "/_alias/mi_lot", None),
        (
            "POST /_aliases index absent",
            "POST",
            "/_aliases",
            {"actions": [{"add": {"index": "fantome_absolu", "alias": "mi_lot"}}]},
        ),
        (
            "POST /_aliases remove absent",
            "POST",
            "/_aliases",
            {"actions": [{"remove": {"index": JOURS[0], "alias": "mi_inconnu"}}]},
        ),
        # --- ecriture a travers un alias
        (
            "alias multi-index en ecriture",
            "POST",
            f"/{ALIAS}/_doc/essai_ecriture",
            {"titre": "essai"},
        ),
        (
            "is_write_index",
            "POST",
            "/_aliases",
            {
                "actions": [
                    {"add": {"index": JOURS[0], "alias": "mi_ecrit"}},
                    {"add": {"index": JOURS[1], "alias": "mi_ecrit", "is_write_index": True}},
                ]
            },
        ),
        (
            "ecriture via is_write_index",
            "PUT",
            # `refresh=true` : sans lui, le `_count` qui suit mesure la vitesse
            # du rafraichissement de fond, pas le moteur.
            "/mi_ecrit/_doc/essai_write?refresh=true",
            {"titre": "essai", "prix": 1.0},
        ),
        ("lecture via alias multi-index", "GET", f"/{ALIAS}/_doc/essai_write", None),
        ("lecture dans l'index d'ecriture", "GET", f"/{JOURS[1]}/_doc/essai_write", None),
        # --- ce qu'un index et un alias ne peuvent pas partager
        ("creer un index du nom d'un alias", "PUT", f"/{ALIAS}", None),
        # --- _count multi-index
        ("_count multi-index", "GET", f"/{','.join(CATALOGUES)}/_count", None),
        ("_count sur alias", "GET", f"/{ALIAS}/_count", None),
        ("_count motif", "GET", f"/{PREFIXE}-2026.08.*/_count", None),
        # --- mapping et refresh sur une expression
        ("GET mapping multi", "GET", f"/{','.join(CATALOGUES)}/_mapping", None),
        ("POST refresh motif", "POST", f"/{PREFIXE}*/_refresh", None),
        ("HEAD index motif", "HEAD", f"/{PREFIXE}-2026.08.*", None),
        ("HEAD motif sans correspondance", "HEAD", "/fantome_*", None),
        # --- suppression : le geste de retention
        ("DELETE d'un alias (refus)", "DELETE", f"/{ALIAS}", None),
        ("DELETE liste", "DELETE", f"/{CATALOGUES[0]},{CATALOGUES[1]}", None),
        # Par defaut, ES 8 **refuse** de supprimer par motif
        # (`action.destructive_requires_name`, passe a `true` en 8.0). Un projet
        # qui purge en `DELETE /audits-2026.07.*` a donc forcement bascule ce
        # reglage — ferrite doit le porter, pas seulement autoriser le motif.
        ("DELETE motif, reglage par defaut", "DELETE", f"/{PREFIXE}-2026.07.*", None),
        (
            "autoriser la suppression par motif",
            "PUT",
            "/_cluster/settings",
            {"persistent": {"action.destructive_requires_name": False}},
        ),
        ("GET /_cluster/settings", "GET", "/_cluster/settings", None),
        ("DELETE motif ancien", "DELETE", f"/{PREFIXE}-2026.07.*", None),
        ("l'index ancien a disparu", "HEAD", f"/{VIEUX}", None),
        ("DELETE motif sans correspondance", "DELETE", "/fantome_*", None),
        ("DELETE _all refuse par defaut", "DELETE", "/_all", None),
        (
            "revenir au reglage par defaut",
            "PUT",
            "/_cluster/settings",
            {"persistent": {"action.destructive_requires_name": True}},
        ),
        ("DELETE motif de nouveau refuse", "DELETE", f"/{PREFIXE}-2026.08.*", None),
        ("les index restants", "GET", f"/_cat/indices/{PREFIXE}*?format=json", None),
        ("GET d'un nom reserve", "GET", "/_route_reservee", None),
        ("GET d'un index en majuscules", "GET", "/MAJUSCULE", None),
    ]


# ---------------------------------------------------------------------------
# Comparaison
# ---------------------------------------------------------------------------

# Ce qui ne peut pas coincider entre deux serveurs : durees, identifiants,
# scores (statistiques locales a chaque index), dates de creation.
VOLATILES = {
    "took",
    "_score",
    "max_score",
    "uuid",
    "index_uuid",
    "creation_date",
    "provided_name",
    "cluster_uuid",
    "id",
    "settings",
    "version",
    "_primary_term",
    "_seq_no",
    "root_cause",
    "reason",
    "node",
    "pri.store.size",
    "store.size",
    "docs.deleted",
}


def normaliser(v, cle=None):
    if cle in VOLATILES:
        return "<volatile>"
    if isinstance(v, dict):
        return {k: normaliser(x, k) for k, x in sorted(v.items())}
    if isinstance(v, list):
        return [normaliser(x, cle) for x in v]
    return v


# Les ecarts **voulus**, avec leur raison. Les compter comme des echecs
# masquerait les vrais ; ne pas les lister du tout laisserait croire a une
# identite qui n'existe pas. Ils sont aussi dans `docs/compat.md`.
#
# La table est vide depuis que ferrite implemente
# `index.query.parse.allow_unmapped_fields` (le reglage d'ES, avec son defaut) :
# « champ inconnu partout » etait la derniere divergence, et un vrai client l'a
# fait tomber — un filtre pose sur chaque recherche, sur un champ pas encore
# mappe, faisait echouer l'application entiere la ou ES rend 0.
DIVERGENCES_ASSUMEES = {}


def presque_egal(a, b, chemin, ecarts):
    if isinstance(a, dict) and isinstance(b, dict):
        for cle in sorted(set(a) | set(b)):
            if cle not in a:
                ecarts.append(f"{chemin}.{cle} : absent de ferrite (ES : {json.dumps(b[cle])[:60]})")
            elif cle not in b:
                ecarts.append(f"{chemin}.{cle} : en trop chez ferrite ({json.dumps(a[cle])[:60]})")
            else:
                presque_egal(a[cle], b[cle], f"{chemin}.{cle}", ecarts)
    elif isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            ecarts.append(f"{chemin} : {len(a)} elements chez ferrite, {len(b)} chez ES")
        for i, (x, y) in enumerate(zip(a, b)):
            presque_egal(x, y, f"{chemin}[{i}]", ecarts)
    elif isinstance(a, float) or isinstance(b, float):
        if a is None or b is None:
            if a is not b:
                ecarts.append(f"{chemin} : ferrite={a} / ES={b}")
        elif abs(float(a) - float(b)) > 1e-9 * max(1.0, abs(float(b))):
            ecarts.append(f"{chemin} : ferrite={a} / ES={b}")
    elif a != b:
        ecarts.append(f"{chemin} : ferrite={json.dumps(a)[:70]} / ES={json.dumps(b)[:70]}")


def resume_recherche(client, kwargs):
    try:
        r = client.search(**kwargs)
    except ApiError as ex:
        return {
            "erreur": True,
            "status": ex.meta.status,
            "type": ex.body.get("error", {}).get("type") if isinstance(ex.body, dict) else None,
        }
    hits = r["hits"]
    return {
        "erreur": False,
        "total": hits["total"],
        # L'ordre compte : c'est lui que la fusion entre index peut casser.
        "docs": [(h["_index"], h["_id"]) for h in hits["hits"]],
        # `_shards` compte les index vises, et signale ceux qui n'ont pas su
        # repondre — un tri sur un champ qu'un index ne mappe pas, par exemple.
        "shards": normaliser(
            {k: v for k, v in r["_shards"].items() if k != "skipped"}
        ),
        "aggregations": r.get("aggregations"),
    }


# ES 8 exige que `Content-Type` et `Accept` s'accordent : poser l'un sans
# l'autre fait echouer l'appel avec `media_type_header_exception`. Cette erreur
# la ne se voit pas en calibrant contre deux Elasticsearch — les deux cotes
# echouent pareil, et la batterie passe au vert sans avoir rien mesure.
ENTETES = {"content-type": "application/json", "accept": "application/json"}


def brut(client, methode, chemin, corps):
    try:
        r = client.perform_request(methode, chemin, headers=ENTETES, body=corps)
        return {"status": r.meta.status, "corps": normaliser(r.body)}
    except ApiError as ex:
        corps_err = ex.body if isinstance(ex.body, dict) else {}
        return {
            "status": ex.meta.status,
            "type": corps_err.get("error", {}).get("type")
            if isinstance(corps_err.get("error"), dict)
            else "<chaine>",
        }


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    calibrer = "--calibrer" in sys.argv
    if calibrer:
        url_ferrite = args[0] if args else "http://localhost:9201"
        url_es = args[1] if len(args) > 1 else "http://localhost:9202"
    else:
        url_ferrite = args[0] if args else "http://localhost:9200"
        url_es = args[1] if len(args) > 1 else "http://localhost:9201"

    gauche = "ES(A)" if calibrer else "ferrite"
    f = Elasticsearch(url_ferrite, request_timeout=180)
    e = Elasticsearch(url_es, request_timeout=180)

    docs = corpus.documents()
    print(f"== corpus : {len(docs)} documents repartis sur {len(TOUS)} index")
    if calibrer:
        print("== calibrage : la batterie contre deux Elasticsearch")
    for client, nom in ((f, gauche), (e, "ES")):
        preparer(client, docs)
        client.indices.put_alias(index=",".join(JOURS), name=ALIAS)
        print(f"   {nom} pret")

    compteurs = {"ok": 0, "assume": 0, "ecart": 0}

    def juger(label, rf, re_):
        ecarts = []
        presque_egal(rf, re_, "", ecarts)
        raison = DIVERGENCES_ASSUMEES.get(label)
        if ecarts and raison:
            compteurs["assume"] += 1
            print(f"  [assume] {label}")
            print(f"           {raison}")
        elif ecarts:
            compteurs["ecart"] += 1
            print(f"  [ecart ] {label}")
            for x in ecarts[:5]:
                print(f"           {x}")
        else:
            compteurs["ok"] += 1
            marque = " (la divergence assumee n'existe plus)" if raison else ""
            print(f"  [  ok  ] {label}{marque}")

    print("\n== recherches\n")
    for label, kwargs in recherches():
        juger(label, resume_recherche(f, kwargs), resume_recherche(e, kwargs))

    print("\n== alias, motifs, suppression\n")
    for label, methode, chemin, corps in apres_coup():
        juger(label, brut(f, methode, chemin, corps), brut(e, methode, chemin, corps))

    for client in (f, e):
        nettoyer(client)
    total = sum(compteurs.values())
    print(f"\n  {compteurs['ok']}/{total} appels identiques a Elasticsearch"
          f", {compteurs['assume']} divergence(s) assumee(s)"
          f", {compteurs['ecart']} ecart(s)")
    return 0 if compteurs["ecart"] == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
