# ferrite

Un moteur de recherche **compatible avec l'API Elasticsearch**, écrit en Rust,
pensé pour tenir dans un conteneur minuscule.

## Pourquoi

Elasticsearch est excellent et surdimensionné pour l'immense majorité des
usages : une JVM, plus d'un gigaoctet de RSS, un démarrage qui se compte en
dizaines de secondes, du tuning de heap — pour indexer quelques centaines de
milliers de documents et répondre à des requêtes `bool` + `terms` + un tri.

`ferrite` vise le même contrat d'API, dans une enveloppe sans commune mesure :

<!-- chiffres-conteneur:apercu — généré depuis docs/container.json par `python3 tests/compat/chiffres_conteneur.py --injecte`, ne pas éditer à la main -->
|  | Elasticsearch 8.15.0 | ferrite |
|---|---|---|
| Image compressée, telle qu'un registre la sert | 669,1 Mo | **4,2 Mo** (`scratch`) |
| RSS au repos | 1,13 Go | **4,7 Mo** |
| Démarrage | 17,4 s | **179 ms** (`docker run` → premier `GET /` servi) |
| Runtime | JVM + tuning heap | un binaire statique |
<!-- /chiffres-conteneur:apercu -->

Ces chiffres sont mesurés, pas visés, et les deux colonnes sont mesurées **de la
même façon** — voir [Le conteneur](#le-conteneur), qui dit aussi pourquoi une
taille d'image sans sa définition ne veut rien dire. Ils ne sont pas saisis
ici : ils sont **générés** depuis [`docs/container.json`](docs/container.json),
le rapport de la campagne, et la CI échoue s'ils en divergent.

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

La ligne d'agrégation qui figurait ici (`terms` + `sum`, 51,77 ms contre 66,88)
a été **retirée** : elle a été mesurée par la campagne qui a trouvé ce défaut,
donc sur un moteur qui ne comptait pas tous les documents de ses buckets rares.
Elle disait le prix d'un calcul faux. La campagne n'a pas été relancée, et
publier un chiffre qu'on ne tient plus coûte plus cher que de ne rien publier.

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
  `exists`, `prefix`, `wildcard`, `regexp`, `nested`…), `sort` (avec `missing`,
  `mode` et `unmapped_type`), `from`/`size`,
  `scroll` (donc `helpers.scan`), filtrage de `_source`.
- **Agrégations** : métriques + `terms` / `date_histogram` / `range` /
  `histogram` / `filter`, avec sous-agrégations. `terms` filtre ses termes
  (`include` / `exclude`, par expression régulière ou liste exacte) et classe
  ses seaux sur une sous-agrégation (`{"order": {"panier_moyen": "desc"}}`) :
  de quoi faire une vraie facette de catalogue. `date_histogram` groupe par
  **mois, trimestre ou année civils** (`calendar_interval`) et dans le fuseau
  qu'on lui donne (`time_zone`) — un jour de changement d'heure y dure 23 ou
  25 heures, comme chez Elasticsearch.
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
tar xzf ferrite-v0.8.0-x86_64-unknown-linux-musl.tar.gz && ./ferrite
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
chaque CI. Elasticsearch 8.15.0 est mesuré par **le même script**, sur la même
machine, dans la même campagne. Et ils ne sont pas recopiés ici : le rapport de
la campagne est un fichier, [`docs/container.json`](docs/container.json), d'où
ce tableau est **généré** — chaque valeur y porte sa définition, et un cliquet
de CI échoue si le tableau et le fichier divergent.

<!-- chiffres-conteneur:tableau — généré depuis docs/container.json par `python3 tests/compat/chiffres_conteneur.py --injecte`, ne pas éditer à la main -->
| | Elasticsearch 8.15.0 | ferrite 0.8.0 | × |
|---|---|---|---|
| **Image compressée**, telle qu'un registre la sert | 669,1 Mo | **4,2 Mo** | **×161** |
| Image décompressée, ce que son système de fichiers occupe | 1 266,1 Mo | 9,8 Mo | ×129 |
| Le binaire seul | — | 9,8 Mo | |
| Mémoire au repos (RSS) | 1,13 Go | **4,7 Mo** | **×239** |
| Démarrage (`docker run` → premier `GET /` servi) | 17,4 s | **179 ms** (médiane de 5 ; l'essentiel est la création du conteneur par Docker) | ×97 |
<!-- /chiffres-conteneur:tableau -->

L'image finale est un `scratch` qui ne contient que le binaire statique — d'où
la troisième ligne, qui est la deuxième à 2 Ko près (l'en-tête `tar` de la
couche). Elle n'a pas de contrepartie chez Elasticsearch, dont l'`ENTRYPOINT`
est un lien vers un script qui lance une JVM : le script l'imprime alors « non
mesuré » plutôt que d'aller chercher un nombre qui ne se comparerait à rien.
Le RSS, lui, est la somme des `VmRSS` de **tous** les processus du cgroup du
conteneur — ferrite est seul dans le sien, mais l'`ENTRYPOINT` d'Elasticsearch
lance un shell qui lance la JVM, et lire le seul PID 1 y aurait rendu le RSS du
shell : deux ordres de grandeur en dessous, et du bon côté pour nous.

**Le chiffre que ce dépôt publie est le premier : l'image compressée, celle
qu'un `docker pull` télécharge.** Les trois sont honnêtes et ne répondent pas à
la même question ; la seule exigence est que les deux colonnes portent la même
définition, ce qui n'était pas le cas jusqu'ici (voir juste en dessous).

<!-- chiffres-conteneur:unites — généré depuis docs/container.json par `python3 tests/compat/chiffres_conteneur.py --injecte`, ne pas éditer à la main -->
Les « Mo » sont des mégaoctets décimaux (10⁶ octets), les mêmes que ceux
qu'affiche `docker images` ; en Mio (2²⁰) la première ligne se lirait
638,1 contre 4,0.
<!-- /chiffres-conteneur:unites -->

### Correction : les 8,2 Mo annoncés jusqu'ici

Cette page a longtemps annoncé « Image : 638 Mo contre 8,2 Mo ». **Les deux
moitiés de cette ligne ne mesuraient pas la même chose** : 638 était la taille
*compressée* d'Elasticsearch, 8,2 la taille *décompressée* de ferrite, et les
deux étaient des Mio affichés sous le nom de Mo. Sous sa propre définition, le
8,2 Mo était juste — c'est ce que `docker image inspect --format '{{.Size}}'`
rendait pour l'image 0.3.0, sur les Docker d'alors. Il est faux aujourd'hui deux
fois :

- **le binaire a grossi** : la même définition rendrait aujourd'hui la ligne
  « décompressée » du tableau ci-dessus ;
- **`.Size` a changé de sens.** Il dépend du **magasin d'images**, donc en
  pratique de la version : avec le magasin classique il rendait la somme des
  couches décompressées, avec celui de containerd — le défaut depuis Docker 29 —
  il rend la somme des blobs compressés. Sur la machine de mesure, le script
  rendait donc la taille compressée pendant que `docker images` en affichait,
  pour la même image, plus du triple — et cette troisième valeur est encore
  autre chose : la colonne `DISK USAGE` additionne les blobs compressés *et*
  leur copie dépliée, que containerd garde tous les deux.

Une image, quatre nombres, aucune définition écrite à côté. C'est pour ça que le
script ne lit plus aucun champ dont le sens dépend de la version de Docker : il
demande l'image (`docker save`, format OCI) et **compte les octets** — la
version du serveur Docker est imprimée quand même, puisqu'elle change ce que les
autres outils répondent à la même question.

<!-- chiffres-conteneur:sortie — généré depuis docs/container.json par `python3 tests/compat/chiffres_conteneur.py --injecte`, ne pas éditer à la main -->
```
$ ./tests/compat/measure_container.sh ferrite:0.8.0
docker serveur   : 29.7.2 (magasin d'images : overlayfs)

== image : ferrite:0.8.0  (linux/amd64, 1 couche(s))
compressee (registre) :     4 151 199 octets      4,2 Mo   <- ce qu'un `docker pull` telecharge
decompressee (disque) :     9 818 112 octets      9,8 Mo   <- ce que le systeme de fichiers de l'image occupe
binaire /ferrite      :     9 816 192 octets      9,8 Mo   <- le fichier que le conteneur execute

demarrage        : 179 ms (docker run -> premier GET / servi, mediane de 5 tours)
RSS au repos     : 4 616 Ko (4,7 Mo)
```
<!-- /chiffres-conteneur:sortie -->

La même commande mesure n'importe quelle image, ce qui est la seule façon de
tenir la ligne « même définition des deux côtés » :

```bash
./tests/compat/measure_container.sh --tailles docker.elastic.co/elasticsearch/elasticsearch:8.15.0
```

### Le premier passage en CI a rendu un chiffre faux, en vert

`docker save` ne rend les blobs **compressés** que depuis le magasin d'images de
containerd. Les runners de la CI sont encore en Docker 28, où il écrit bien un
layout OCI — mais avec des **couches nues**, et un manifeste qui déclare leur
taille décompressée. Le script a donc imprimé « compressée (registre) :
9 520 806 octets », c'est-à-dire la taille décompressée sous le nom de l'autre,
sans un mot et dans un job vert. Le défaut que cette page corrige, reproduit
par sa propre correction.

La question se pose maintenant **aux octets de chaque couche** : si l'une d'elles
n'est pas compressée, la taille qu'un registre servirait n'est pas déductible, et
le script la refuse (`NON MESURABLE`, code de retour non nul) plutôt que de la
remplacer par un nombre plausible. Recompresser pour combler le trou mesurerait
notre `gzip`, pas celui du registre.

La CI produit donc l'artefact OCI directement — c'est exactement ce qu'un
`docker push` enverrait, et il sort du cache du build :

```bash
docker buildx build --output type=oci,dest=/tmp/ferrite-oci.tar .
IMAGE_TAR=/tmp/ferrite-oci.tar ./tests/compat/measure_container.sh ferrite:ci
```

Les deux chemins ont été étalonnés l'un contre l'autre, et c'est ce qui permet
de croire celui que la CI publie. Le **blob de la couche est identique à
l'octet** — 4 005 821 des deux côtés, soit tout ce qu'un `docker pull`
télécharge vraiment. Les totaux diffèrent de **6 octets**, entièrement dans le
JSON de configuration de l'image (1 295 contre 1 289) : ce sont deux builds
distincts, et l'horodatage qu'ils y écrivent n'a pas la même longueur. D'où le
chiffre publié en Mo et non à l'octet : les six octets sont une propriété du
build, pas de l'image.

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

**Et les clients officiels ne se contentent plus de se connecter : leurs suites
de tests tournent dessus.** « Le client officiel se branche » est une capture
d'écran ; « la suite de tests du client officiel passe » est une preuve. Trois
clients, licence Apache-2.0 vérifiée dans le clone, révision figée, arbre
vérifié intact : `go-elasticsearch` v8.13.0 passe 16/30 de sa suite
d'intégration contre ferrite là où un vrai Elasticsearch 8.15 en passe 28/30,
chaque écart rattaché à une capacité déclarée. Et le **cycle de vie du client** —
découverte de version, en-tête `X-elastic-product`, négociation de compression,
sniffing (ou son refus propre), erreurs typées, helpers (`bulk`,
`streaming_bulk`, `scan`) — passe **entièrement dans les trois langages**, avec
le client publié sur PyPI, npm et le proxy de modules Go. Ce que ça a trouvé, et
ce que ça n'a pas mesuré : [`docs/clients.md`](docs/clients.md).

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
`date_histogram`, et sous-agrégations — de quoi construire des facettes et des
graphes temporels. « Par mois » se demande par `calendar_interval` et non par
« 30 jours », et « par jour à Paris » par `time_zone` : les seaux suivent alors
l'heure locale, changements d'heure compris — l'arrondi d'Elasticsearch est
reproduit et mesuré contre le sien sur 25 914 cas et 603 fuseaux. Une
facette de catalogue, pas seulement un comptage : `terms` filtre ses termes
(`include` / `exclude`, par expression régulière de Lucene ou par liste exacte)
et classe ses seaux sur ce qu'ils contiennent — « les dix catégories au panier
moyen le plus élevé » s'écrit `{"order": {"panier_moyen": "desc"}}`, et c'est la
seule forme d'ordre qui demande de calculer les sous-agrégations avant de trier.
Y compris sur les buckets **rares**, ce qui n'a pas toujours été vrai : au-delà de
2 048 documents par segment, une sous-agrégation sous un `terms` ou un `range`
perdait leurs documents, en 200 et avec le bon `doc_count` à côté. ferrite
épingle le correctif d'amont de tantivy pour ça — ce qu'il contient et comment
en sortir : [`docs/tantivy-patch.md`](docs/tantivy-patch.md).

Les **analyzers** `standard`, `simple`, `whitespace`, `keyword`, `stop`,
`english` et `french` sont vérifiés identiques à ceux d'ES sur 217 textes —
**positions et offsets compris** — et `_analyze` permet de le constater.

Les **analyzers de langue** aussi, désormais : `danish`, `dutch`, `german`,
`hungarian`, `italian`, `norwegian`, `portuguese`, `romanian`, `russian`,
`spanish`, `swedish`, `turkish` et `snowball`. Ils étaient refusés au motif que
« le stemmer de tantivy n'est pas celui de Lucene » ; la mesure a montré que
c'est **faux pour huit d'entre eux** — zéro écart sur les vocabulaires du projet
Snowball, 563 000 mots que personne n'a écrits ici. Ce qui manquait était
ailleurs : les listes de mots vides, quatre filtres (normalisation allemande,
élision, apostrophe et minuscules turques) et les stemmers **légers** que
l'allemand, l'espagnol, l'italien et le portugais d'ES posent à la place de
Snowball. Le tableau de l'écart, étape par étape, est dans
[`docs/compat.md`](docs/compat.md).

Le **finnois** reste refusé, et il est chiffré : le stemmer disponible s'écarte
de celui de Lucene sur **13 mots des 84 399** du vocabulaire finnois (0,015 %),
tous des emprunts à diacritique étranger. Un analyzer n'est jamais livré sous le
nom d'ES tant qu'il n'est pas mesuré identique.

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

**Le tuning de pertinence** — « le même match, mais les articles récents
devant », « les produits en stock devant », « ne pas exclure les archives,
juste les repousser » — passe désormais : `function_score` (avec `weight`,
`field_value_factor` et ses dix `modifier`, les décroissances `gauss` / `exp` /
`linear` sur un champ numérique ou une date, `score_mode`, `boost_mode`,
`max_boost`, `min_score`) et `boosting`. Ces deux clauses ne rendent pas un
ensemble de documents : elles rendent une **valeur**, et une formule recopiée
depuis la documentation rend un nombre plausible qu'aucune lecture ne distingue
d'un nombre juste. Les trois briques de calcul sont donc verrouillées contre
**les classes d'Elasticsearch elles-mêmes**, exécutées dans le conteneur de
référence : 58 476 points rejoués par `cargo test`, sans Docker
([`genere_scoring.py`](tests/compat/genere_scoring.py)). `random_score` et
`script_score` sont refusés en les nommant.

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
requête supportée à 90 % est une requête qui échoue. Réponse : **96,4 % des
requêtes trouvées dans du code d'application**, 40,4 % des exemples de la
documentation, 47,2 % des tracks de benchmark. L'écart entre ces trois nombres
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

**Deux suites de tests écrites ailleurs**, pas une : celle d'Elasticsearch
7.10.2 (la dernière sous Apache 2.0) et celle d'**OpenSearch** 2.19.1 (Apache 2.0
aussi), jouées toutes les deux par le même runner, sans liste blanche. Une seule
serait un examen dont on connaît le sujet — et celle d'Elastic est figée en
2020, donc aveugle à tout ce qui a été ajouté depuis. Les deux rangeaient chacune
36 échecs en « régression » et se recoupent sur 12 capacités ; celle d'OpenSearch
en a sorti trois de plus, toutes postérieures à 2020 — et une fois comblées, son
compte tombe à 32 pendant que celui d'Elastic ne bouge **pas d'un cas**, ce qui
est la démonstration la plus courte de ce que coûte une source unique. Et « ce
n'est pas un défaut de
ferrite, les deux moteurs divergent » ne s'y décrète pas : la même suite est
jouée contre un vrai Elasticsearch 8.15, et un cas qu'il échoue lui aussi est
rangé à part — mesuré, pas décidé. Voir
[`docs/conformance.md`](docs/conformance.md).

Cet inventaire est aussi ce qui **borne un tirage au sort**. Un fuzzer
différentiel ([`tests/compat/fuzz_vs_es.py`](tests/compat/fuzz_vs_es.py)) génère
des mappings, des documents et des requêtes dans le périmètre que `compat.yaml`
déclare, les pose à ferrite **et** à un vrai Elasticsearch 8.15, et compare les
réponses champ par champ : **670 cas, 30 937 requêtes, 4 divergences ouvertes**
— toutes **antérieures** à la dernière carte, et mesurées telles — sur quatre
plages de graines dont **aucune** n'a servi à corriger, et **83 divergences**
sur les mêmes plages contre le binaire d'avant cette carte, ce qui est la seule
chose qui dit que ces plages mesurent quelque chose. Il
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
