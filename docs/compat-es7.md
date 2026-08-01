# Un projet resté en Elasticsearch 7.10.2 peut-il basculer sur ferrite ?

Il y a deux questions derrière celle-là, et elles n'ont pas la même réponse.

| | Question | Réponse |
|---|---|---|
| **Le client** | mon code 7.x se connecte-t-il et parle-t-il à ferrite ? | **Oui**, et ce qui casse est le coût de la migration 7→8, pas ferrite |
| **L'instance** | ferrite peut-il remplacer mon Elasticsearch 7.10.2 ? | **Sur les requêtes, oui — à l'identique.** Sur les index et les documents, seulement s'ils sont **plats** |

Le point dur n'est donc pas la version : c'est le périmètre de ferrite.
**Un document contenant un sous-objet est refusé** (`{"fournisseur": {…}}` comme
`[{…}]`), et un mapping 7.x traîne presque toujours des analyzers sur mesure,
un `format` de date ou un objet. En revanche, sur un corpus identique,
ferrite rend **les mêmes documents dans le même ordre** que l'instance 7.10.2.

Si les documents ont des sous-objets, la suite dépend de **lequel** des trois
mécanismes d'Elasticsearch est en jeu — `object`, `nested` ou `join` : le
premier est un chantier cadré, les deux autres sont un mur. Voir
[la section dédiée](#object-nested-join--trois-choses-différentes), et
`diff_es7.py --inventaire` pour le savoir sur une instance donnée.

Les deux moitiés de ce fichier traitent les deux questions séparément, parce
qu'elles ne se corrigent pas au même endroit.

# Première question : le client

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

# Seconde question : l'instance

Reprendre une instance 7.10.2, c'est trois choses distinctes : ses **index**,
ses **documents**, ses **requêtes**. `tests/compat/diff_es7.py` les prend dans
cet ordre, contre une vraie instance 7.10.2 lancée à côté :

```bash
docker run -d --name es7 -p 9201:9200 -e discovery.type=single-node \
  docker.elastic.co/elasticsearch/elasticsearch:7.10.2
cargo run &
python3 tests/compat/diff_es7.py http://localhost:9200 http://localhost:9201
```

Le script **lit** l'instance (export des définitions, `scan` des documents) et
n'écrit que sur ferrite ; seule la phase de comparaison des requêtes indexe un
corpus des deux côtés, et `--sans-ecriture` la désactive. Il est donc utilisable
tel quel contre une instance qui compte : ce qu'il rapporte, ce sont **vos**
index, pas ceux d'un exemple.

## Les requêtes : identiques (137/138)

C'est le résultat qui rassure, et c'est le cœur du sujet.

Même corpus de 600 documents des deux côtés, les **138 requêtes** de
`diff_relevance.py` (`match`, `multi_match`, `match_phrase`, `bool`, `dis_max`,
`term(s)`, `range`, `exists`, `prefix`, `fuzzy`, `constant_score`, tris)
rejouées sur les deux serveurs :

```
  137/138 requetes : memes documents, meme ordre qu'ES 7.10.2
  1/138 : ordre permute uniquement entre ex aequo d'ES 7
  0/138 refusees par ferrite, 0/138 ecarts reels
```

**Zéro écart réel.** Le seul déplacement porte sur deux documents auxquels
Elasticsearch attribue lui-même le même score. C'est exactement le résultat que
donne la même batterie contre un ES **8.15.0** : la fidélité de ferrite ne se
dégrade pas quand la référence est une 7.10.2.

## Les documents : seulement s'ils sont plats

C'est le blocage réel, et il n'a rien à voir avec la version.

```
== Phase 2 — transfert des documents (scan 7.x -> bulk ferrite)
  [KO] legacy_7x : 0/2 documents transferes
       2 refus : ferrite ne supporte pas les champs objet/imbriques :
                 [fournisseur] dans l'index [migre_legacy_7x]
```

Un document dont un champ vaut `{"nom": "Atelier"}` — ou `[{"nom": …}]` — est
refusé. Les noms pointés (`"fournisseur.nom": "Atelier"`) le sont aussi. Un
document 7.x doit donc être **aplati côté client** (`fournisseur_nom`) pour
entrer dans ferrite, ce qui n'est plus « le code client ne change pas ».

À vérifier en premier sur l'instance : est-ce que les documents ont des
sous-objets ? Si oui, la migration demande une transformation, pas un transfert.

### `object`, `nested`, `join` : trois choses différentes

Elasticsearch a trois façons de représenter « un objet dans un document », et
elles ne coûtent pas du tout la même chose à reprendre. Mesuré des deux côtés :

| Ce que le mapping déclare | ES 7.10.2 | ferrite |
|---|---|---|
| sous-objet implicite (`{"fournisseur": {"nom": …}}`, sans mapping) | mapping `fournisseur.properties.nom`, requête `term fournisseur.nom` | refus explicite à l'indexation |
| sous-objet déclaré (`"fournisseur": {"properties": {…}}`) | accepté | `ferrite ne supporte pas les champs objet/imbriques` |
| `"type": "object"` | accepté | `ferrite ne supporte pas le type de champ [object]` |
| `"type": "nested"` + requête `nested` | accepté | `ferrite ne supporte pas le type de champ [nested]` |
| `"type": "join"` + `has_child` / `has_parent` | accepté | `ferrite ne supporte pas le type de champ [join]` |
| champ déjà aplati (`fournisseur_nom`) | accepté | **accepté** |

Pour savoir lequel des trois est en jeu sur une instance — et combien de fois —
sans rien y écrire ni même lancer ferrite :

```bash
python3 tests/compat/diff_es7.py --inventaire http://mon-es:9200
```

```
  structures :
      3  object (sous-objet)    ex. blog:auteur
      1  join (parent/enfant)   ex. blog:lien
      1  nested                 ex. commandes:lignes
```

**Ce que chacun demanderait à ferrite**, pour que l'arbitrage se fasse sur des
faits :

- **`object` — faisable, et pas énorme.** Elasticsearch n'invente rien ici : un
  sous-objet *est* un ensemble de chemins pointés (`client.ville`), et c'est
  ainsi qu'il l'indexe. Or ferrite indexe **déjà** par chemin pointé : ses
  multi-fields (`titre.keyword`) sont des champs tantivy à part entière, et tout
  le Query DSL, les tris et les agrégations résolvent un champ par son chemin
  (`Fields.mapped`, une table `chemin → champ`). Le support demande donc de
  parcourir récursivement le mapping et le document au lieu de leur premier
  niveau, et de re-nicher les chemins dans la réponse `_mapping`. Le reste du
  moteur n'a pas à bouger.
- **`nested` — un autre moteur.** La sémantique de `nested` (chaque sous-objet
  interrogé comme une unité, pour que `qte > 10` et `ref = "A"` portent sur *la
  même* ligne) repose chez Lucene sur des documents cachés et une jointure de
  bloc (`ToParentBlockJoinQuery`). tantivy n'a pas d'équivalent. C'est un projet,
  pas une itération — et l'aplatir en `object` donnerait des résultats **faux**
  sans le dire, ce que ce projet refuse.
- **`join` — hors périmètre.** Parent/enfant, c'est la même famille de jointures,
  plus le routage. Rien dans un déploiement mono-conteneur ne le rend
  indispensable.

Autrement dit : des documents à sous-objets ordinaires sont un chantier
identifié ; des mappings `nested` ou `join` sont, eux, un mur.

> Ce transfert a mis au jour un bug, corrigé dans la même PR : un champ valant
> `[{…}]` était **accepté en silence** (gardé dans `_source`, absent du mapping,
> donc introuvable) là où l'objet nu était refusé. Il est désormais refusé de la
> même façon.

## Les index : hébergeables dégradés

La définition exportée d'un index 7.x typique (analyzer sur mesure, analyzer
`french`, `format` de date, sous-objet, `refresh_interval`) est rejouée sur
ferrite en pelant les couches. Aucune ne passe telle quelle ; ce qui reste :

```
  [ok] legacy_7x -> cree sur ferrite (mappings nettoyes (7/8 champs))
       retire : champ [fournisseur]  — champs objet/imbriques
       retire : champ [cree_le]      — parametre [format]
       retire : champ [description]  — analyzer [french]
       retire : champ [titre]        — analyzer [fr_produit] (settings.analysis)
```

Trois familles de refus, par ordre de gêne :

1. **`settings.analysis` et les analyzers de langue** — ce n'est pas un détail
   de configuration : un champ `text` analysé en `french` n'est pas indexé de la
   même façon en `standard`, donc les résultats changeront. `compat.md` explique
   pourquoi le refus est assumé (le stemmer de tantivy n'est pas celui de
   Lucene) ; la conséquence pratique est qu'un index 7.x qui s'appuie sur un
   analyzer de langue **ne se réplique pas à l'identique** aujourd'hui.
2. **Les sous-objets** — voir ci-dessus.
3. **Les paramètres de champ décoratifs** (`format` sur une date, etc.) et tous
   les réglages d'index (`refresh_interval`, allocation…) : ceux-là se retirent
   sans conséquence sur les résultats.

Les réglages « privés » que l'export contient (`index.uuid`, `creation_date`,
`provided_name`, `version`, `routing.allocation.*`) sont refusés par
Elasticsearch lui-même à la création : les retirer fait partie de n'importe
quelle migration, y compris d'un ES 7 vers un ES 8.

Rien ne se transfère au niveau fichier : ferrite ne lit pas un index Lucene, et
les `_snapshot` ne sont pas implémentés. Une migration passe forcément par une
réindexation depuis la source (`scan` + `bulk`), ce que fait la phase 2.

## La forme des réponses : seul `_type` manque

Sur les réponses qu'un code 7.x lit vraiment, clé par clé :

| Réponse | Écart avec l'instance 7.10.2 |
|---|---|
| indexation, `get`, `delete`, item de `_bulk`, hit de recherche | **`_type` absent** (il n'existe plus en 8.x) |
| enveloppe `hits` (`total`, `max_score`, `_shards`, `timed_out`) | aucun |
| `_cluster/health` | aucun |

Deux différences de *valeur* à connaître en plus des clés : `_shards.total` vaut
1 chez ferrite (mono-shard) contre 2 sur un ES par défaut, et `hits.total` est
**toujours exact** là où ES plafonne à 10 000 (`relation: "gte"`). Un code qui
teste `relation == "gte"` continue de fonctionner, il ne verra simplement jamais
ce cas.
