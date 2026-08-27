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
| Image | ~1,3 Go | **8,2 Mo** (`scratch`) |
| RSS au repos | > 1 Go | **3,6 Mo** |
| Démarrage | 30-60 s | **225 ms** (`docker run` → premier `GET /` servi) |
| Runtime | JVM + tuning heap | un binaire statique |

Ces chiffres sont mesurés, pas visés — voir [Le conteneur](#le-conteneur).

### Et une fois qu'il y a des documents dedans ?

Un banc sur quelques centaines de documents ne mesure qu'un aller-retour HTTP.
Celui-ci tourne sur le corpus public de la track Rally
[`geonames`](https://github.com/elastic/rally-tracks/tree/b1cc31cd1afd68dbc0a0bebfef3a17ebd3747d79/geonames)
d'Elastic, avec **ses** requêtes, à **2 000 000 de documents**, les deux
serveurs en conteneur sur la même machine :

| | Elasticsearch 8.15 | ferrite |
|---|---|---|
| `match_phrase` (latence médiane / p95) | 3,13 / 3,92 ms | **1,20 / 1,43 ms** |
| `term` | 2,58 / 3,25 ms | **1,55 / 1,91 ms** |
| Agrégation `terms` + `sum` | 66,88 ms | **51,77 ms** |
| RSS | 3,40 Go | **425 Mo** |
| Tri sur `match_all` | **4,00 ms** | 1 178 ms |
| `scroll`, 25 pages × 1 000 | **433 ms** | 1 737 ms |
| Indexation | **51 484 doc/s** | 10 198 doc/s |
| Taille sur disque (ES fusionné) | **403 Mo** | 473 Mo |

Les quatre dernières lignes sont là exprès : **ferrite perd sur le tri, sur
l'export, sur l'indexation et sur le disque**, et le tri est le pire résultat du
banc — jusqu'à ×290. Un banc qui ne montre que des victoires n'est pas lu comme
un banc. Le protocole, la seconde échelle (500 000 documents), les 18 requêtes
de la track que ferrite refuse, **le défaut de sous-agrégation que ce banc a
trouvé** — des valeurs fausses en 200 dans les buckets rares, corrigé depuis
(voir [`docs/tantivy-patch.md`](docs/tantivy-patch.md)) — et **la taille au-delà
de laquelle il n'est plus le bon choix** sont dans
[`docs/bench.md`](docs/bench.md).

L'argument n'est pas « on refait Elasticsearch en mieux ». C'est : **le code
client existant ne change pas** (mêmes bibliothèques officielles, mêmes
requêtes, mêmes mappings), et le déploiement devient un détail — un sidecar, un
conteneur de CI, un binaire embarqué dans une image applicative, un
environnement de dev qui démarre instantanément.

Pour reprendre le projet — la méthode, les décisions déjà tranchées, les pièges
déjà payés : [`CLAUDE.md`](CLAUDE.md).

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
  sémantique de `refresh`, et la modification **par requête**
  (`_delete_by_query`, `_update_by_query`).
- **Mappings** : types de base, multi-fields (`.keyword`), analyzers
  déclaratifs, `_source`, mapping dynamique.
- **Recherche** : le noyau du Query DSL (`bool`, `match`, `multi_match` (avec
  `lenient` et les types `phrase` / `phrase_prefix`), `match_phrase`,
  `match_phrase_prefix`, `term(s)`, `range` avec le **date math** (`now`),
  `exists`, `prefix`, `wildcard`, `regexp`, `nested`…), `sort`, `from`/`size`,
  `scroll` (donc `helpers.scan`), filtrage de `_source`.
- **Agrégations** : métriques + `terms` / `date_histogram` / `range` /
  `histogram` / `filter`, avec sous-agrégations.
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
tar xzf ferrite-v0.7.0-x86_64-unknown-linux-musl.tar.gz && ./ferrite
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

| | Elasticsearch 8.15.0 | ferrite 0.7.0 |
|---|---|---|
| Image | 638 Mo | **8,2 Mo** |
| Mémoire au repos | 1,02 Gio | **3,6 Mo** (RSS) |
| Démarrage (`docker run` → premier `GET /` servi) | 22,9 s | **225 ms** (l'essentiel est la création du conteneur par Docker) |

L'image finale est un `scratch` qui ne contient que le binaire statique.

## État

**Deux vraies applications tournent dessus, sans être modifiées.**
[Gitea](https://github.com/go-gitea/gitea) v1.27.2 — la forge Git — indexe ses
issues dans Elasticsearch et les cherche avec `bool`, `multi_match`, `term`,
`terms`, `range` et un tri : sa propre suite d'intégration passe ses **34 cas**,
exactement les mêmes que contre un vrai Elasticsearch 8.15.0.
[Wagtail](https://github.com/wagtail/wagtail) v7.1 — un CMS Django, qui passe
par le client officiel `elasticsearch-py` 8.x, pose ses propres analyzers à
n-grammes et se refait un `_all` — passe les **83 tests** de sa suite de
backend. Dans les deux cas l'arbre du dépôt est vérifié intact avant de
conclure.

Ce qu'il a fallu corriger pour y arriver n'était, la plupart du temps, **pas un
manque de moteur** : `index: true`, le défaut d'ES qu'ES lui-même ne conserve
pas ; tout nom de champ commençant par `_`, qu'ES n'interdit que pour ses
propres métadonnées ; `{"bool": {"mustNot": …}}`, l'écriture dépréciée qu'ES
sert encore. Aucun de ces refus de trop n'était visible depuis le corpus de
5 311 requêtes ni depuis la suite REST d'Elastic : une application ne commence
pas par une recherche, elle commence par créer son index. La recette et le
relevé de ce que ces applications envoient sont dans
[`docs/application.md`](docs/application.md).

**Ce qui marche** : un client Elasticsearch officiel non modifié crée un index
avec un mapping explicite, indexe des documents via `_bulk`, et les retrouve via
`_search` — `match`, `multi_match`, `match_phrase`, `match_phrase_prefix`,
`term`, `terms`, `range`, `exists`, `ids`, `prefix`, `wildcard`, `regexp`,
`fuzzy`, `bool`, `constant_score`, `dis_max`, `match_all` — avec scoring BM25, `from`/`size`, tri, filtrage de
`_source`, et le format de réponse exact d'ES.

Sur un corpus de 600 documents et 213 requêtes, ferrite et un vrai
Elasticsearch 8.15 renvoient **les mêmes documents dans le même ordre**
(`tests/compat/diff_relevance.py`).

Le **mapping dynamique** et les **multi-fields** (`titre.keyword`) sont
supportés : on peut rejouer le mapping d'un Elasticsearch existant, ou indexer
sans rien déclarer.

Les **agrégations** sont là aussi : métriques, `terms`, `range`, `histogram`,
`date_histogram`, et sous-agrégations — de quoi construire des facettes. Y
compris sur les buckets **rares**, ce qui n'a pas toujours été vrai : au-delà de
2 048 documents par segment, une sous-agrégation sous un `terms` ou un `range`
perdait leurs documents, en 200 et avec le bon `doc_count` à côté. ferrite
épingle le correctif d'amont de tantivy pour ça — ce qu'il contient et comment
en sortir : [`docs/tantivy-patch.md`](docs/tantivy-patch.md).

Les **analyzers** `standard`, `simple`, `whitespace`, `keyword`, `stop`,
`english` et `french` sont vérifiés identiques à ceux d'ES sur 217 textes —
**positions et offsets compris** — et `_analyze` permet de le constater. Les
autres langues restent **refusées** : leur stemmer n'est pas porté, et porter le
nom d'ES en indexant autre chose changerait silencieusement les résultats.

Un index peut aussi **déclarer son analyse** (`settings.analysis`) : ses propres
analyzers `custom`, ses tokenizers et ses filtres. Les **n-grammes** (`ngram`,
`edge_ngram`) en font partie, côté tokenizer comme côté filtre — c'est la brique
de l'**autocomplétion « au fil de la frappe »**, celle qui travaille à
l'indexation là où `match_phrase_prefix` travaille à la requête.
`index.max_ngram_diff` est honoré, avec le message d'ES. Son compagnon obligé
l'est aussi : `search_analyzer` fait chercher le **mot entier** là où on a
indexé des grammes — sans lui, `elan` rend tout ce qui commence par `e`, chez ES
comme ici.

Le mapping sert aussi `copy_to` — recopier la valeur brute d'un champ dans une
ou plusieurs cibles à l'indexation, la façon dont on se refait un `_all` — et
`store`, qui conserve la valeur à part du `_source` pour que `stored_fields` la
relise sans rapatrier tout le document.

Côté API de documents : `_update` (fusion partielle, `upsert`), `_mget`,
`_count`, l'action `update` du `_bulk`, le versionnage optimiste
(`if_seq_no`/`if_primary_term`) et `PUT _mapping` pour ajouter des champs.

La recherche porte sur **une expression d'index**, comme chez ES :
`es.search(index=["produits", "marques"])`, `logs-2026.08.*`, `_all`,
`logs-*,-logs-2026.07.*`, ou un **alias**. Les alias sont gérés
(`POST /_aliases`, `is_write_index`, bascule atomique), et la purge d'une
rétention par index quotidien s'écrit `DELETE /logs-2026.07.*` — refusée par
défaut, comme sur un ES 8, tant que `action.destructive_requires_name` n'a pas
été basculé.

Les **bornes de date** d'une requête sont des expressions, comme chez ES :
`{"range": {"fin": {"lt": "now"}}}` — le filtre « en retard » de n'importe quel
tableau de bord — est résolu côté serveur, `now-1d/d` et `2026-03-15||+1M` aussi.
Et une borne est arrondie **du côté où elle est** : `lte: "2026-03-15"` couvre la
journée entière, `lt: "2026-03-15"` s'arrête à minuit. 276 bornes mesurées
identiques à ES 8.15 (`tests/compat/diff_datemath.py`).

L'**export d'un index** marche avec le code que tout le monde écrit :
`helpers.scan` du client officiel, donc `?scroll=1m` et `/_search/scroll`. Le
contexte fige l'index : ce qui est écrit pendant l'export ne s'y invite pas, et
chaque document sort une fois et une seule.

Les **petites routes** qui bloquent un outil entier passent aussi :
`_field_caps` (ce que chaque champ sait faire, et son type index par index —
c'est ce qu'appelle un outil de découverte de champs), `_validate/query`,
`_stats`, `PUT /{index}/_settings` et les **templates d'index**, dans leurs deux
familles (`_index_template` et le `_template` déprécié qu'on trouve dans les
scripts d'init venus de la 7.x). Un template s'applique à la création implicite
de l'index comme à sa création explicite.

**Ce que la réponse transporte** se choisit aussi : `fields` — la façon que la
7.10+ met en avant, et celle qu'envoie Kibana — `docvalue_fields` et
`stored_fields`. Les trois ne lisent pas au même endroit, et c'est tout le
sujet : `fields` lit le `_source` (il garde donc l'ordre du document et ses
doublons), `docvalue_fields` lit les colonnes (donc trié, et dédoublonné sur un
`keyword`), `stored_fields` lit les champs que le mapping déclare `store: true`
— et rien pour les autres, exactement ce que rend un Elasticsearch dont le
mapping ne le porte pas. La **forme** est ce qui compte pour un client : chaque
valeur est un tableau, même pour un champ mono-valué, et un champ absent n'a pas
de clé.

**Modifier ou purger par requête** — ce qu'un script de maintenance fait tous
les jours — passe désormais : `_delete_by_query` (purger les documents d'un
locataire, retirer un lot par filtre) et `_update_by_query`, qui **sans script**
réindexe depuis le `_source`, c'est-à-dire exactement le geste d'après un
`PUT /_mapping`. Les deux rendent les compteurs d'ES — `total`, `deleted` /
`updated`, `batches`, `version_conflicts`, `failures[]` — et les
`version_conflicts` ne sont pas un ornement : la commande relève chaque document
sur l'instantané de la recherche puis n'écrit **que s'il n'a pas bougé depuis**,
comme ES. `conflicts=proceed` les absorbe, `abort` (le défaut) s'arrête à la fin
du lot fautif et rend 409. Ce qui reste refusé, par son nom : `script`
(Painless), `slices`, `wait_for_completion=false` (il rendrait une tâche, et
ferrite n'a pas d'API `_tasks`) et `requests_per_second`.

**Les fragments surlignés d'une barre de recherche** (`highlight`) sont rendus,
et ce qui a coûté le travail n'est pas de marquer les termes : c'est de couper
les fragments **là où Lucene les coupe**. Un fragment n'est ni « une phrase » ni
« `fragment_size` caractères » — les phrases sont fusionnées vers l'avant tant
que la longueur tient, puis re-coupées au mot ; un point suivi d'une minuscule
ne termine pas une phrase ; et quand il y en a plus que demandé, ce sont les
mieux notés par le `PassageScorer` de Lucene qui restent, remis dans l'ordre du
document. Le tout est mesuré fragment par fragment contre un ES 8.15
([`diff_highlight.py`](tests/compat/diff_highlight.py)). Ce qui n'est pas
reproduit est refusé en le nommant : `type`, `highlight_query`,
`matched_fields`, `boundary_scanner`, `encoder`, `order: score`.

**Ce qui n'y est pas encore** : `search_after`, `_msearch`,
`_reindex`, `query_string`, les templates de composants
(`_component_template`), les champs calculés par un script Painless
(`script_fields`, `runtime_mappings` — leur objet **vide** est accepté, il ne
demande rien) et les analyzers des autres langues.

L'inventaire complet — supporté, partiel, refusé, et les divergences assumées —
est dans [`docs/compat.md`](docs/compat.md). Rien de ce qui n'est pas supporté
n'échoue en silence : chaque clause, type ou route inconnu produit une erreur
explicite au format d'Elasticsearch.

**Ce que ça vaut, pondéré par l'usage.** Un pourcentage de cas de test ne dit
rien de ce qu'une application peut brancher : il met `bool` + `match` au même
rang qu'un `significant_terms` avec script. Sur un corpus de **5 311 requêtes
réelles** — la documentation de référence d'ES 8.15, les tracks Rally d'Elastic,
les tests des clients officiels et le code de 184 dépôts open source — la
question posée est « celle-ci passerait-elle **entièrement** ? », parce qu'une
requête supportée à 90 % est une requête qui échoue. Réponse : **93,2 % des
requêtes trouvées dans du code d'application**, 39,8 % des exemples de la
documentation, 28,6 % des tracks de benchmark. L'écart entre ces trois nombres
est le résultat ; la méthode, les sources et les biais sont dans
[`docs/usage.md`](docs/usage.md), le corpus est publié avec.

Cet inventaire n'est plus écrit à la main : sa source est
[`compat.yaml`](compat.yaml) à la racine — une entrée par capacité, avec son
état et, pour un refus, son **motif** (hors périmètre assumé / pas encore /
divergence de moteur / comme ES). `docs/compat.md` et sa forme machine
[`docs/compat.json`](docs/compat.json) en sont générés, et la CI échoue s'ils
ne correspondent plus. C'est le même fichier que lit le rapport de conformance
pour dire, d'un cas qui échoue, s'il porte sur une capacité qu'on **annonce**
(une régression) ou sur une capacité qu'on **refuse** (le coût du périmètre).

Cet inventaire est aussi ce qui **borne un tirage au sort**. Un fuzzer
différentiel ([`tests/compat/fuzz_vs_es.py`](tests/compat/fuzz_vs_es.py)) génère
des mappings, des documents et des requêtes dans le périmètre que `compat.yaml`
déclare, les pose à ferrite **et** à un vrai Elasticsearch 8.15, et compare les
réponses champ par champ : **2 000 cas, 88 367 requêtes, 1 divergence ouverte**
(un ordre que BM25 sépare, déclarée) sur huit plages de graines, dont cinq
jamais utilisées pour corriger. Il
s'étalonne d'abord contre deux Elasticsearch — tant qu'il n'y est pas à zéro, ce
qu'il dit de ferrite ne vaut rien. Son premier passage a trouvé vingt et un défauts
que personne n'avait signalés, tous silencieux ; ils sont racontés dans
[`docs/fuzz.md`](docs/fuzz.md) et figés dans
[`tests/compat/sonde_fuzz.py`](tests/compat/sonde_fuzz.py).

Le fonctionnement du pipeline idée→prod est décrit dans
[`docs/dev-workflow.md`](docs/dev-workflow.md).

## Nom

`ferrite` = oxyde de fer. Aucune affiliation avec Elastic N.V. ; « compatible
avec l'API Elasticsearch » décrit une interface, Elasticsearch est une marque
déposée d'Elastic N.V.
