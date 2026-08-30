"""Le nettoyage inter-cas de la suite serveur d'`elasticsearch-py`, remplace.

Charge par `-p nettoyage_compatible` (donc **a cote** de la suite, jamais
dedans : aucun fichier du clone n'est touche, et `tests_clients.py` le verifie
par `git diff` apres coup).

# Pourquoi

`test_elasticsearch/utils.wipe_cluster` fait le menage entre chaque cas. Ce
n'est pas un test : c'est la fixture. Elle passe par dix-huit sondes dont
**seize** demandent des routes qu'un moteur de recherche n'a pas a servir —
rollup, SLM, ILM, CCR, ML, transform, snapshots, data streams, `_tasks`,
`_cluster/pending_tasks`, `_cat/templates?h=name`, `_component_template`,
`_nodes/shutdown`, `_cluster/state`.

Resultat mesure : la suite telle quelle rend **0 cas vert et 82 erreurs** contre
ferrite, toutes levees dans la meme fixture, avant que le moindre test ne
commence. Ce chiffre est publie tel quel — il dit une chose vraie, qu'une suite
de client suppose un cluster complet. Mais il ne dit rien de ce que les tests
mesurent.

Ce plugin repose donc le menage sur les seules routes que **les deux** serveurs
servent, et la suite est relancee : c'est la colonne « adapte ». Les deux
colonnes sont publiees cote a cote, et les seize routes ecartees sont nommees
une par une dans le rapport. Une adaptation qu'on ne compte pas est une
adaptation qui grandit.

# Ce qu'il ne fait pas

Il ne touche pas aux index systeme (`.` en tete) : un vrai Elasticsearch
reinstalle les siens, donc les supprimer ne ferait que du bruit — et la colonne
de reference doit etre mesuree dans les memes conditions que l'autre, sinon
elle n'etalonne rien.
"""
import sys


def nettoie(client):
    """Index, templates des deux familles, reglages de cluster. Rien d'autre —
    et rien qui suppose une brique qu'un des deux serveurs n'a pas."""
    try:
        from elasticsearch import AsyncElasticsearch, Elasticsearch

        if isinstance(client, AsyncElasticsearch):
            # La suite asynchrone passe son client asynchrone a une fonction
            # synchrone : c'est ce que fait `wipe_cluster` d'origine, pour la
            # meme raison (le menage n'a pas a etre asynchrone).
            config = client.transport.node_pool.get().config
            client = Elasticsearch([config], verify_certs=False)
    except ImportError:
        pass

    noms = [
        ligne["index"]
        for ligne in client.cat.indices(format="json", expand_wildcards="all")
        if not ligne["index"].startswith(".")
    ]
    if noms:
        client.options(ignore_status=404).indices.delete(
            index=",".join(noms), expand_wildcards="all"
        )

    for tpl in client.indices.get_index_template().get("index_templates", []):
        if not tpl["name"].startswith("."):
            client.options(ignore_status=404).indices.delete_index_template(name=tpl["name"])
    for nom in client.indices.get_template():
        if not nom.startswith("."):
            client.options(ignore_status=404).indices.delete_template(name=nom)

    reglages = client.cluster.get_settings()
    remise = {
        portee: {cle + ".*": None for cle in valeurs}
        for portee, valeurs in reglages.items()
        if valeurs
    }
    if remise:
        client.cluster.put_settings(body=remise)


def pytest_configure(config):
    """Repose la fonction la ou la suite l'a deja importee.

    Les conftest de la suite font `from ..utils import wipe_cluster` : le nom y
    est **lie par valeur** au chargement, et ce chargement a lieu avant
    `pytest_configure`. Repatcher le seul module `utils` ne suffirait donc pas —
    et l'oubli serait silencieux, la suite continuant d'appeler l'ancienne.
    """
    combien = 0
    for module in list(sys.modules.values()):
        if module is not None and getattr(module, "wipe_cluster", None) is not None:
            module.wipe_cluster = nettoie
            combien += 1
    print(f"[nettoyage_compatible] repose sur {combien} modules", file=sys.stderr)
    if combien == 0:
        raise RuntimeError(
            "nettoyage_compatible n'a trouve aucun `wipe_cluster` a remplacer : "
            "la suite a change, et la mesure ne mesurerait plus ce qu'elle dit"
        )
