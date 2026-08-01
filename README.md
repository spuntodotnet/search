# ferrite

Un moteur de recherche **compatible avec l'API Elasticsearch**, écrit en Rust,
pensé pour tenir dans un conteneur minuscule.

## Pourquoi

Elasticsearch est excellent et surdimensionné pour l'immense majorité des
usages : une JVM, plus d'un gigaoctet de RSS, 30 à 60 secondes de démarrage, du
tuning de heap — pour indexer quelques centaines de milliers de documents et
répondre à des requêtes `bool` + `terms` + un tri.

`ferrite` vise le même contrat d'API, dans une enveloppe sans commune mesure :

|  | Elasticsearch | ferrite |
|---|---|---|
| Image | ~1,3 Go | **2,4 Mo** (`scratch`) |
| RSS au repos | > 1 Go | **2,9 Mo** |
| Démarrage | 30-60 s | **11 ms** (~230 ms via `docker run`) |
| Runtime | JVM + tuning heap | un binaire statique |
| Latence de recherche (médiane / p95) | 4,90 / 7,00 ms | **1,41 / 1,98 ms** |
| Débit (8 requêtes en vol) | 779 req/s | **1 172 req/s** |

Ces chiffres sont mesurés, pas visés — voir [Le conteneur](#le-conteneur) pour
l'enveloppe, et `tests/compat/bench_vs_es.py` pour les deux dernières lignes :
mêmes 600 documents et mêmes 138 requêtes des deux côtés, dont **137 rendent
exactement les mêmes documents dans le même ordre** (la 138ᵉ permute deux
ex æquo). Le banc se lance contre n'importe quel Elasticsearch, 7.x ou 8.x.

L'argument n'est pas « on refait Elasticsearch en mieux ». C'est : **le code
client existant ne change pas** (mêmes bibliothèques officielles, mêmes
requêtes, mêmes mappings), et le déploiement devient un détail — un sidecar, un
conteneur de CI, un binaire embarqué dans une image applicative, un
environnement de dev qui démarre instantanément.

## Périmètre

Le moteur d'index inversé n'est pas réécrit : `ferrite` s'appuie sur
[**Tantivy**](https://github.com/quickwit-oss/tantivy) (l'équivalent Rust de
Lucene — postings compressés, FST, doc values colonnaires, BM25). Le travail
réel est la **couche de compatibilité** au-dessus.

### Dans le périmètre

- API HTTP compatible **Elasticsearch 8.x** — les clients officiels
  (`elasticsearch-py`, `-js`, `-go`) doivent fonctionner sans modification. Un
  client **7.x** se connecte sans rien changer lui aussi ; ce qui casse alors,
  c'est ce que la 8 a supprimé — inventaire mesuré dans
  [`docs/compat-es7.md`](docs/compat-es7.md).
- **Ingestion** : `_doc`, `_create`, `_update`, `_mget`, `_bulk` (NDJSON),
  sémantique de `refresh`.
- **Mappings** : types de base, multi-fields (`.keyword`), analyzers
  déclaratifs, `_source`, mapping dynamique.
- **Recherche** : le noyau du Query DSL (`bool`, `match`, `match_phrase`,
  `term(s)`, `range`, `exists`, `prefix`, `nested`…), `sort`, `from`/`size`,
  `search_after`, `highlight`, filtrage de `_source`.
- **Agrégations** : métriques + `terms` / `date_histogram` / `range` /
  `filters`, avec sous-agrégations.
- **Mono-nœud assumé** : les routes de cluster (`_cluster/health`, `_cat/*`,
  `_nodes`) répondent de façon crédible et constante — un shard, zéro réplique,
  toujours `green`.

### Hors périmètre (explicitement)

Sharding, réplication, consensus, réallocation, recovery distribué. Scripting
Painless. Machine learning, alerting, sécurité par rôles. Ce sont ces morceaux
qui font le coût réel d'Elasticsearch, et ce sont exactement ceux dont un
déploiement mono-conteneur n'a pas besoin.

## Démarrer

```bash
cargo run                       # ferrite écoute sur :9200
# ou
docker build -t ferrite . && docker run --rm -p 9200:9200 -v ferrite-data:/data ferrite
```

Sans rien compiler : chaque version publie un binaire statique Linux **x86-64**
et **arm64** dans les [releases](../../releases) (archive `.tar.gz` + `.sha256`).

```bash
tar xzf ferrite-v0.1.0-x86_64-unknown-linux-musl.tar.gz && ./ferrite
```

Puis, avec le client officiel — sans une ligne de code spécifique à ferrite :

```python
from elasticsearch import Elasticsearch

es = Elasticsearch("http://localhost:9200")
es.indices.create(index="livres", mappings={"properties": {
    "titre": {"type": "text"}, "auteur": {"type": "keyword"},
    "annee": {"type": "integer"}}})
es.index(index="livres", id="1", refresh=True,
         document={"titre": "Bel-Ami", "auteur": "Maupassant", "annee": 1885})
es.search(index="livres", query={"match": {"titre": "bel ami"}})
```

Variables d'environnement : `FERRITE_BIND` (défaut `0.0.0.0:9200`),
`FERRITE_DATA` (`./data`), `FERRITE_CLUSTER_NAME`, `FERRITE_NODE_NAME`.

## Le conteneur

Les chiffres ci-dessous sont mesurés, pas visés — par
[`tests/compat/measure_container.sh`](tests/compat/measure_container.sh), à
chaque CI. Elasticsearch 8.15.0 est mesuré sur la même machine, dans les mêmes
conditions.

| | Elasticsearch 8.15.0 | ferrite 0.1.0 |
|---|---|---|
| Image | 638 Mo | **2,4 Mo** |
| Mémoire au repos | 1,02 Gio | **2,9 Mo** (RSS) |
| Démarrage (`docker run` → premier `GET /` servi) | 22,9 s | **232 ms** (11 ms pour le binaire seul : le reste est la création du conteneur par Docker) |

L'image finale est un `scratch` qui ne contient que le binaire statique.

## État

**Ce qui marche** : un client Elasticsearch officiel non modifié crée un index
avec un mapping explicite, indexe des documents via `_bulk`, et les retrouve via
`_search` — `match`, `multi_match`, `match_phrase`, `term`, `terms`, `range`,
`exists`, `ids`, `prefix`, `wildcard`, `fuzzy`, `bool`, `constant_score`,
`dis_max`, `match_all` — avec scoring BM25, `from`/`size`, tri, filtrage de
`_source`, et le format de réponse exact d'ES.

Sur un corpus de 600 documents et 138 requêtes, ferrite et un vrai
Elasticsearch 8.15 renvoient **les mêmes documents dans le même ordre**
(`tests/compat/diff_relevance.py`).

Le **mapping dynamique** et les **multi-fields** (`titre.keyword`) sont
supportés : on peut rejouer le mapping d'un Elasticsearch existant, ou indexer
sans rien déclarer.

Les **agrégations** sont là aussi : métriques, `terms`, `range`, `histogram`,
`date_histogram`, et sous-agrégations — de quoi construire des facettes.

Les **analyzers** `standard`, `simple`, `whitespace`, `keyword` et `stop` sont
vérifiés token par token identiques à ceux d'ES, et `_analyze` permet de le
constater. Les analyzers de langue (`french`, `english`…) sont **refusés** :
le stemmer de tantivy n'est pas celui de Lucene, et porter le nom d'ES en
indexant autre chose changerait silencieusement les résultats.

Côté API de documents : `_update` (fusion partielle, `upsert`), `_mget`,
`_count`, l'action `update` du `_bulk`, le versionnage optimiste
(`if_seq_no`/`if_primary_term`) et `PUT _mapping` pour ajouter des champs.

**Ce qui n'y est pas encore** : `highlight`, `search_after`, analyzers de langue
et sur mesure, `_update_by_query` / `_reindex`, `query_string`, recherche
multi-index, alias.

L'inventaire complet — supporté, partiel, refusé, et les divergences assumées —
est dans [`docs/compat.md`](docs/compat.md). Rien de ce qui n'est pas supporté
n'échoue en silence : chaque clause, type ou route inconnu produit une erreur
explicite au format d'Elasticsearch.

Le fonctionnement du pipeline idée→prod est décrit dans
[`docs/dev-workflow.md`](docs/dev-workflow.md).

## Nom

`ferrite` = oxyde de fer. Aucune affiliation avec Elastic N.V. ; « compatible
avec l'API Elasticsearch » décrit une interface, Elasticsearch est une marque
déposée d'Elastic N.V.
