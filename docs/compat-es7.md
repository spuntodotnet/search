# Un projet resté en Elasticsearch 7.10.2 peut-il basculer sur ferrite ?

**Réponse courte : oui, la connexion et le gros du code passent tels quels — le
travail n'est pas de « faire marcher ferrite », c'est de faire la migration
7.x → 8.x que le projet n'a pas encore faite.** ferrite annonce l'API 8.15.0 :
tout ce qu'Elastic a supprimé entre 7 et 8 est cassé chez ferrite exactement
comme ça le serait sur un vrai Elasticsearch 8. À cela s'ajoutent les manques
propres à ferrite, qui, eux, ne dépendent pas de la version.

Ce fichier sépare les deux, parce qu'ils ne se corrigent pas au même endroit.

## Comment ces réponses ont été obtenues

`tests/compat/probe_es7.py` — 32 scénarios écrits comme du code de projet 7.x
(client officiel `elasticsearch-py` 7.10.1, tout par `body=`, `doc_type=`,
`helpers.scan`) — a été exécuté contre **trois** serveurs :

```bash
python3 tests/compat/probe_es7.py http://localhost:9200   # ferrite
python3 tests/compat/probe_es7.py http://localhost:9201   # elasticsearch:7.10.2
python3 tests/compat/probe_es7.py http://localhost:9202   # elasticsearch:8.15.0
```

| Serveur | Résultat |
|---|---|
| Elasticsearch **7.10.2** | 32 OK / 0 KO |
| Elasticsearch **8.15.0** | 26 OK / **6 KO** |
| **ferrite** | 21 OK / **11 KO** |

Les 6 KO d'ES 8.15 sont le coût de la migration 7→8, indépendant de ferrite.
Les 5 KO restants sont les manques de ferrite.

Le client 7.10.2 n'est pas publié sur PyPI (7.10.0, 7.10.1 puis 7.11 le sont) :
le 7.10.1 a été utilisé, c'est le même code de transport. Le **serveur** de
référence est bien un `docker.elastic.co/elasticsearch/elasticsearch:7.10.2`.

## Ce qui marche : la connexion, d'abord

C'est la question qui décide de tout le reste, et elle est réglée.

- `Elasticsearch("http://…")`, `ping()`, `info()` : ✅ avec le client **7.10.1**
  comme avec le **7.17.9**. Un client 7.x ne refuse pas un serveur qui annonce
  `8.15.0` — la vérification de version côté client n'existe pas en 7.10, et le
  contrôle produit introduit en 7.14 se contente du header
  `X-elastic-product: Elasticsearch`, que ferrite pose sur toutes ses réponses.
- Aucun besoin de `compatibility mode` ni de header spécial côté client.

Marchent aussi, sans modification, avec le client 7.x :

`indices.create(body={"mappings": …})` · `indices.get_mapping()` ·
`indices.put_mapping(body=…)` · `index(body=…)` (avec ou sans
`doc_type="_doc"`) · `get` · `update(body={"doc": …})` · `delete` ·
`mget(body={"docs": …})` · `bulk(body=[…])` sans `_type` · `helpers.bulk()` ·
`search(body={"query": …})` avec `bool` / `filter` / `range` / `sort` /
`_source` · `track_total_hits` · les agrégations · `count(body=…)` ·
`cluster.health()` · `cat.indices(format="json")` · `indices.refresh()` ·
`indices.exists` / `delete`.

## Ce qui casse parce que 7 ≠ 8 (et casserait pareil sur un vrai ES 8)

C'est la moitié la plus importante : ces points ne sont **pas** des manques de
ferrite. Les corriger, c'est faire la migration 7→8 — et le code corrigé
fonctionnera aussi bien contre un Elasticsearch 8 que contre ferrite.

| Code 7.x | ferrite | ES 8.15.0 | Correction |
|---|---|---|---|
| `es.index(index, doc_type="livre", …)`, URL `/{index}/{type}/{id}` | 400 `no handler found` | 400 `no handler found` | passer aux routes `_doc` : `/{index}/_doc/{id}` |
| `_type` dans une action `_bulk` | 400, métadonnée refusée | 400 `unknown parameter [_type]` | retirer `_type` des lignes d'action |
| `include_type_name=True` à la création d'index | 400 paramètre inconnu | 400 (mapping niché sous `_doc`) | mapping à plat : `{"mappings": {"properties": …}}` |
| `include_type_name=…` sur `put_mapping` | 400 paramètre inconnu | 400 paramètre inconnu | retirer le paramètre |
| lecture de `hit["_type"]`, `réponse["_type"]` | champ **absent** | champ **absent** | le champ n'existe plus ; supprimer ces lectures |
| `es.search(doc_type="_doc", …)` → `/{index}/_doc/_search` | ⚠️ voir ci-dessous | ⚠️ voir ci-dessous | retirer `doc_type` des appels de recherche |
| `es.count(doc_type="_doc", …)` → `/{index}/_doc/_count` | ⚠️ idem | ⚠️ idem | idem |

### ⚠️ Le piège à connaître avant de brancher quoi que ce soit

`search(doc_type=…)` construit l'URL `POST /{index}/_doc/_search`. En 7.10 c'est
une recherche typée (dépréciée mais fonctionnelle). À partir de la 8, cette URL
n'est plus une recherche : c'est **l'indexation d'un document dont l'`_id` est
`_search`**. Le corps de la requête est écrit dans l'index, et la réponse est un
`201 created`.

Constaté sur un vrai Elasticsearch 8.15.0 :

```
KO  search(doc_type='_doc', ...) → la requête a été INDEXÉE comme document _id=_search
    {"_index": "probe7", "_id": "_search", "_version": 1, "result": "created", …}
```

ferrite fait la même chose, et c'est bien là le problème :

```
$ curl -XPOST 'localhost:9200/silence/_doc/_search' -d '{"size":1}'
{"_index":"silence","_id":"_search","_version":1,"result":"created", …}
```

Dans le probe, ferrite renvoie une erreur là où ES 8 indexe — mais par accident :
le corps de recherche contenait `{"query": {…}}` et ferrite refuse les champs
objet. Avec un corps plat (`{"size": 1}`, `{"from": 0}`) l'écriture passe des
deux côtés. **Un code 7.x qui pagine avec `doc_type` pollue donc l'index au lieu
de chercher, sans que rien ne le signale.** C'est le seul point de cette page
qui mérite une vérification avant migration : `grep -rn "doc_type" ` sur le code
client.

ferrite se conforme ici à Elasticsearch 8 plutôt qu'à sa propre règle « jamais
d'échec silencieux ». Refuser un `_id` commençant par `_` sur les routes de
document est une divergence assumée possible — elle n'est pas prise
unilatéralement, c'est une décision produit.

## Ce qui casse parce que c'est ferrite

Ces manques n'ont rien à voir avec la 7.10.2 : un projet déjà en 8.x les
rencontrerait à l'identique. Ils sont listés ici parce qu'ils pèsent lourd dans
du code 7.x, où ils sont particulièrement idiomatiques.

| Appel 7.x | ferrite | ES 8.15.0 | Contournement |
|---|---|---|---|
| `search(scroll="1m")` + `scroll()` | 400 paramètre `scroll` inconnu | ✅ | pas de `scroll` (voir `docs/compat.md`) ; pagination par `from`/`size` sous `max_result_window` |
| `helpers.scan(es, …)` | 400 (repose sur `scroll`) | ✅ | idem — **c'est le manque le plus visible** : `scan` est le raccourci le plus utilisé dans du code 7.x |
| `search(rest_total_hits_as_int=True)` | 400 paramètre inconnu | ✅ (`"total": 4`) | lire `hits.total.value` ; ce paramètre est du compat 6.x, il n'y a aucune raison technique de le refuser |
| `msearch(body=[…])` | 400 route non implémentée | ✅ | boucler sur `search` |
| `indices.stats()` | 400 `no handler found` | ✅ | `cat.indices(format="json")` donne le nombre de documents |
| `indices.put_template` / `get_settings` / `put_settings` / alias | 400 | ✅ | non implémentés (`docs/compat.md`) |
| `update_by_query` / `delete_by_query` / `reindex` | 400 refus explicite | ✅ | réindexer côté client |
| `search(index="a*")`, `search(index="a,b")`, `_search` global | 400 refus explicite | ✅ | nommer un index unique |
| `settings.analysis` (analyzers sur mesure), analyzers de langue (`french`, `english`) | 400 refus explicite | ✅ | analyzers intégrés seulement (`standard`, `simple`, `whitespace`, `keyword`, `stop`) |

Le tableau complet du périmètre est dans [`compat.md`](compat.md) — cette page
n'en extrait que ce qu'un code 7.x heurte en premier.

## En résumé, ce qu'il faut regarder dans le projet

Par ordre décroissant de coût, sur le code client :

1. `grep -rn "helpers.scan\|scroll" ` — pas d'équivalent chez ferrite
   aujourd'hui. Si le projet parcourt des index entiers, c'est le point bloquant.
2. `grep -rn "doc_type" ` — sur les recherches, cette écriture **pollue l'index
   en silence** (ES 8 fait pareil).
3. `grep -rn '\["_type"\]' ` — le champ n'existe plus dans les réponses.
4. `grep -rn "include_type_name\|rest_total_hits_as_int" ` — paramètres refusés.
5. `grep -rn "analysis\|analyzer" ` dans les mappings — seuls les analyzers
   intégrés existent, et aucun analyzer de langue.
6. Les index cherchés par motif ou par liste, et `msearch`.

Rien dans cette liste ne dépend du fait que le serveur actuel soit un 7.10.2
plutôt qu'un 7.17 : ce qui compte, c'est que le code n'a pas encore vu la 8.
