# Le banc, à l'échelle

> La mesure : [`tests/compat/bench_echelle.py`](../tests/compat/bench_echelle.py).
> Le rapport machine, d'où sort chaque chiffre de cette page :
> [`bench.json`](bench.json).

## Pourquoi cette page existe

Le README annonçait une latence médiane, un p95 et un débit face à
Elasticsearch. Ces chiffres étaient mesurés — sur **600 documents et 138
requêtes écrites ici**. À cette taille, l'index tient entier dans le cache du
processeur : on mesure surtout le coût d'un aller-retour HTTP. Et le corpus
comme les requêtes étaient les nôtres, donc un dénominateur choisi par celui
qui publie le résultat — exactement ce que ce dépôt refuse partout ailleurs
(voir [`conformance.md`](conformance.md), [`usage.md`](usage.md)).

Cette page remplace ces chiffres. Le corpus et les requêtes viennent d'ailleurs,
les deux serveurs reçoivent la même chose, et **ce que ferrite perd est publié
avec ce qu'il gagne**. Un banc qui ne montre que des victoires n'est pas lu
comme un banc.

## Le protocole

| | |
|---|---|
| Corpus | track Rally [`geonames`](https://github.com/elastic/rally-tracks/tree/b1cc31cd1afd68dbc0a0bebfef3a17ebd3747d79/geonames) d'Elastic (Apache-2.0), révision `b1cc31cd1afd68dbc0a0bebfef3a17ebd3747d79` |
| Documents | les `N` premiers de `documents-2.json.bz2` (11 396 503 au total), téléchargé depuis `rally-tracks.elastic.co` et vérifié à l'octet près (265 208 777 octets compressés, la taille que `track.json` déclare) |
| Échelles | **500 000** et **2 000 000** de documents |
| Mapping | celui de la track, **lu** dans son `index.json` — pas retapé ici (six écarts, listés plus bas) |
| Requêtes | les 31 opérations de recherche de son `operations/default.json`, dont les trois requêtes à 45 586 termes que son `track.py` fabrique depuis `terms.txt` |
| Indexation | 8 clients en parallèle, lots de 5 000 documents — les défauts de la track |
| ferrite | image `scratch` du dépôt, `docker run -p 9200:9200 -v …:/data` — **4,1 Mo compressés** au sens de [l'enveloppe](#lenveloppe-de-limage), contre 669,1 Mo pour celle d'Elasticsearch |
| Elasticsearch | `docker.elastic.co/elasticsearch/elasticsearch:8.15.0`, `discovery.type=single-node`, `ES_JAVA_OPTS=-Xms2g -Xmx2g` |
| Machine | la même pour les deux, l'un après l'autre : 8 cœurs, 31 Gio de RAM, disque local |

Ce qui rend la mesure relisable, et qui manquait à la précédente :

- **le tri entre « jouable » et « refusé » est mesuré, pas déclaré.** Chaque
  opération de la track est posée à ferrite ; s'il la refuse, le refus est
  relevé tel quel et rattaché à une capacité de [`compat.yaml`](../compat.yaml)
  via [`perimetre.py`](../tests/compat/perimetre.py). Choisir soi-même les
  requêtes qu'on mesure, c'est écrire son propre dénominateur ;
- **on attend qu'Elasticsearch ait fini de fusionner** ses segments avant de
  chronométrer quoi que ce soit — c'est ce que fait la track
  (`wait-until-merges-finish`). Sans ça, les fusions d'après-indexation d'ES
  tourneraient pendant qu'on mesure ferrite : on mesurerait la machine ;
- **le `_forcemerge` vient après les chronomètres, jamais avant.** Un index
  fusionné en un seul segment cherche plus vite ; le faire d'un seul côté avant
  de mesurer donnerait à ES un tour d'avance que le tableau ne dirait pas.

### Les six écarts avec la track, et pourquoi

La track ne tourne pas telle quelle contre ferrite. Chaque écart est appliqué
**des deux côtés** sauf mention, et imprimé par l'outil (`--inventaire`) :

| Écart | Côté | Pourquoi |
|---|---|---|
| champ `location` (`geo_point`) retiré du mapping | les deux | `geo_point` est hors du périmètre déclaré. La valeur reste dans le `_source` : les documents envoyés sont identiques à l'octet près, seule l'indexation du champ disparaît |
| `dynamic: strict` → `dynamic: false` | les deux | conséquence de la ligne précédente : en `strict`, un document portant `location` serait rejeté |
| `fielddata: true` retiré de `country_code` | les deux | paramètre de champ refusé par ferrite ; il ne sert dans la track qu'à rendre agrégeable un `text`, et toutes les requêtes du banc passent par `country_code.raw` |
| `number_of_shards` : 5 → 1 | les deux | ferrite est mono-shard. Comparer 1 shard à 5 comparerait des nombres de shards, pas des moteurs |
| `index.requests.cache.enable: false` | ES seul | c'est la track qui coupe le cache de requêtes. ferrite refuse le réglage — il n'a aucun cache de requêtes — donc l'appliquer d'un seul côté **rapproche** les deux serveurs |
| pas de `_forcemerge` avant la mesure | les deux | ferrite n'a pas la route (cycle de vie d'index, hors périmètre). La taille sur disque d'ES est relevée **en plus** après un `_forcemerge`, parce que c'est un gain que ferrite ne sait pas aller chercher |

## Ce que la track demande, et ce que ferrite en sert

Sur les **31** opérations de recherche de la track, **13 tournent des deux
côtés** et 18 sont hors mesure — toutes parce que ferrite les refuse
explicitement, et toutes rattachées à une capacité déclarée refusée
(**aucune régression**) :

| Ce qui est refusé | Opérations | Capacité |
|---|---|---|
| `function_score`, `script_score` | 7 | `dsl.non_supportees` |
| `significant_text`, `sampler` | 4 | `agg.non_supportees` |
| `search_after` | 3 | `recherche.non_supportes` |
| `track_total_hits: false` | 2 | `recherche.track_total_hits` |
| `pre_filter_shard_size` | 2 | `recherche.reglages_de_shards` |

Ces 18 refus sont le **coût du périmètre**, pas un accident : sept d'entre eux
demandent d'exécuter un script (Painless ou `expression`), quatre une
agrégation de recherche de termes significatifs, deux un réglage qui n'a de sens
qu'avec plusieurs shards. Le reste — `search_after` et
`track_total_hits: false` — est ce qu'un vrai projet peut regretter, et ça se
lit ici plutôt que dans une note de bas de page.

## Les chiffres

Deux échelles, la même machine, les deux serveurs l'un après l'autre. Le
rapport `×` compare **toujours** ferrite à Elasticsearch dans le même sens :
au-dessus de 1, ferrite est devant.

### L'enveloppe de l'image

Avant de mesurer ce que les deux serveurs coûtent en marche, ce qu'ils coûtent à
l'arrêt. Ces trois lignes sont mesurées par
[`measure_container.sh`](../tests/compat/measure_container.sh) et **pas** par le
banc ; elles sont ici parce qu'une taille d'image publiée sans sa définition est
un chiffre qu'on ne peut pas vérifier — et parce que ce dépôt en a publié trois
différents pour la même image. Le tableau est **généré** depuis le rapport de la
campagne, [`docs/container.json`](container.json), où chaque valeur porte sa
définition.

<!-- chiffres-conteneur:enveloppe — généré depuis docs/container.json par `python3 tests/compat/chiffres_conteneur.py --injecte`, ne pas éditer à la main -->
| | ferrite 0.7.0 | ES 8.15.0 | × |
|---|---|---|---|
| **Image compressée**, telle qu'un registre la sert | **4,1 Mo** | 669,1 Mo | **×164** |
| Image décompressée, ce que son système de fichiers occupe | 9,7 Mo | 1 266,1 Mo | ×131 |
| Le binaire seul | 9,7 Mo | — (une JVM) | |
<!-- /chiffres-conteneur:enveloppe -->

Le chiffre publié est le premier — **ce qu'un `docker pull` télécharge**. Les Mo
sont décimaux (10⁶ octets), comme ceux de `docker images`. Le README annonçait
« 638 Mo contre 8,2 Mo » : deux définitions différentes sur la même ligne, en
Mio sous le nom de Mo, et un `docker image inspect --format '{{.Size}}'` dont le
sens a changé entre Docker 28 et Docker 29. Le détail de la correction est dans
[le README](../README.md#le-conteneur).

### L'enveloppe, à l'échelle

| | 500 000 documents | | | 2 000 000 documents | | |
|---|---|---|---|---|---|---|
| | ferrite | ES 8.15 | × | ferrite | ES 8.15 | × |
| Indexation (s) | 44,3 | **8,5** | ×0,19 | 196,1 | **38,8** | ×0,20 |
| Indexation (doc/s) | 11 298 | **58 736** | ×0,19 | 10 198 | **51 484** | ×0,20 |
| Taille sur disque, telle qu'indexée | 139,9 Mo | **130,5 Mo** | ×0,93 | **472,7 Mo** | 558,6 Mo | ×1,18 |
| — ES après `_forcemerge` | — | **115,9 Mo** | ×0,83 | — | **402,8 Mo** | ×0,85 |
| RSS | **108,2 Mo** | 3,22 Go | ×30,5 | **425,0 Mo** | 3,40 Go | ×8,2 |
| Débit (8 en vol, req/s) | 5,7 | **39,7** | ×0,14 | 2,7 | **53,0** | ×0,05 |

Deux lignes demandent une lecture, pas un coup d'œil :

- **la taille sur disque change de camp à deux millions de documents** — et ce
  n'est pas un gain de ferrite. Elasticsearch a indexé cinq fois plus vite, donc
  il laisse derrière lui des segments qu'il n'a pas encore fusionnés ; la ligne
  suivante montre ce qu'il devient une fois `_forcemerge` passé (402,8 Mo,
  contre 472,7 Mo pour ferrite, qui n'a pas la route). L'ordre « tel qu'indexé »
  est un instantané, pas un état stable ;
- **le débit est celui du mélange de la track**, où quatre requêtes sur treize
  coûtent près d'une seconde à ferrite. Il dit ce que rend ce mélange-là, pas ce
  que rend une barre de recherche. Le détail par requête, juste en dessous, est
  la ligne qu'il faut lire.

### La latence, requête par requête

Médiane / p95 en millisecondes, `n` mesures par serveur (100 au plus, ou ce que
le budget de 60 s a permis). Le p99 n'est publié dans
[`bench.json`](bench.json) que lorsqu'il y a 100 mesures : un p99 sur quarante
mesures est le maximum déguisé en centile.

#### 500 000 documents

| Requête | ferrite méd. / p95 | ES méd. / p95 | × | n (f/ES) |
|---|---|---|---|---|
| `phrase` (`match_phrase`) | **1,40** / 1,58 | 2,33 / 2,92 | **×1,66** | 100/100 |
| `term` | **1,38** / 1,65 | 2,07 / 3,03 | **×1,50** | 100/100 |
| `default` (`match_all`) | 2,11 / 3,06 | 2,14 / 2,98 | ×1,01 | 100/100 |
| `country_agg_uncached` † | 22,10 / 22,87 | **17,58** / 19,61 | ×0,80 | 100/100 |
| `country_agg_cached` † | 22,05 / 22,61 | **14,17** / 18,80 | ×0,64 | 100/100 |
| `scroll` (25 pages × 1 000) | 1 498,72 / 1 511,27 | **515,22** / 536,58 | ×0,34 | 41/100 |
| `large_terms` (45 587 termes) | 957,06 / 976,34 | **120,66** / 129,59 | ×0,13 | 63/100 |
| `large_filtered_terms` | 959,11 / 970,04 | **114,12** / 122,06 | ×0,12 | 63/100 |
| `large_prohibited_terms` | 958,69 / 970,67 | **108,41** / 115,99 | ×0,11 | 63/100 |
| `desc_sort_population` | 170,44 / 174,62 | **3,84** / 4,35 | ×0,023 | 100/100 |
| `asc_sort_population` | 169,17 / 173,77 | **2,94** / 3,38 | ×0,017 | 100/100 |
| `desc_sort_geonameid` | 273,73 / 282,57 | **4,50** / 6,30 | ×0,016 | 100/100 |
| `asc_sort_geonameid` | 254,89 / 265,54 | **3,45** / 3,92 | ×0,014 | 100/100 |

† **à jeter** : mesuré sur un ferrite dont les sous-agrégations ne comptaient pas
tous les documents de leurs buckets rares. Ces deux lignes disent le prix d'un
calcul faux, corrigé depuis, et ne se comparent plus à rien —
[détail plus bas](#ce-que-la-correction-coûte-et-ce-que-les-deux-lignes-du-tableau-valent-encore).

#### 2 000 000 de documents

| Requête | ferrite méd. / p95 | ES méd. / p95 | × | n (f/ES) |
|---|---|---|---|---|
| `phrase` (`match_phrase`) | **1,20** / 1,43 | 3,13 / 3,92 | **×2,61** | 100/100 |
| `term` | **1,55** / 1,91 | 2,58 / 3,25 | **×1,67** | 100/100 |
| `country_agg_uncached` † | **51,77** / 55,71 | 66,88 / 73,38 | **×1,29** | 100/100 |
| `country_agg_cached` † | **52,15** / 61,92 | 66,52 / 69,68 | **×1,28** | 100/100 |
| `default` (`match_all`) | 5,00 / 7,96 | **3,34** / 3,84 | ×0,67 | 100/100 |
| `scroll` (25 pages × 1 000) | 1 737,06 / 1 773,03 | **432,68** / 458,18 | ×0,25 | 35/100 |
| `large_filtered_terms` | 1 029,87 / 1 058,20 | **144,88** / 152,23 | ×0,14 | 59/100 |
| `large_prohibited_terms` | 1 111,50 / 1 131,80 | **145,74** / 153,01 | ×0,13 | 54/100 |
| `large_terms` (45 587 termes) | 1 022,79 / 1 063,89 | **125,85** / 137,04 | ×0,12 | 59/100 |
| `desc_sort_geonameid` | 1 264,03 / 1 292,18 | **11,69** / 12,32 | ×0,009 | 48/100 |
| `desc_sort_population` | 727,25 / 740,62 | **4,93** / 5,66 | ×0,007 | 83/100 |
| `asc_sort_population` | 721,78 / 738,01 | **2,88** / 3,16 | ×0,004 | 84/100 |
| `asc_sort_geonameid` | 1 177,58 / 1 206,89 | **4,00** / 4,27 | ×0,003 | 51/100 |

† **à jeter**, pour la même raison qu'à l'échelle précédente —
[détail plus bas](#ce-que-la-correction-coûte-et-ce-que-les-deux-lignes-du-tableau-valent-encore).

## Ce que ferrite gagne

- **La recherche filtrée** — un `term`, une phrase — est devant, et son avance
  **grandit** avec l'index : ×1,50 et ×1,66 à cinq cent mille documents, ×1,67
  et ×2,61 à deux millions. C'est le chemin dont dépend une barre de recherche
  ou un filtre d'application, et c'est celui que le produit vend.
- **La mémoire** : 425 Mo contre 3,40 Go à deux millions de documents, 108 Mo
  contre 3,22 Go à cinq cent mille. Le rapport se resserre avec la taille (×30
  puis ×8) parce que la consommation de ferrite grandit avec le nombre de
  documents là où celle d'Elasticsearch est d'abord un tas fixé à l'avance — ici
  `-Xmx2g`, et il ne le rend jamais.
- **L'agrégation `terms` avec sous-agrégation** change de camp entre les deux
  échelles : ×0,80 à cinq cent mille, **×1,29** à deux millions. Ces deux
  chiffres sont à jeter : ils ont été mesurés sur un moteur qui, à cette taille,
  rendait des **valeurs fausses** dans cette agrégation-là. Le défaut est
  corrigé depuis (voir la réserve plus bas), la campagne pas relancée.

## Ce que ferrite perd, et de combien

- **Le tri est le pire résultat de ce banc : ×40 à ×290.** Un `match_all` trié
  sur `geonameid` coûte 4,00 ms à Elasticsearch et 1 178 ms à ferrite sur deux
  millions de documents. Ce n'est pas une surprise, c'est la limite déjà écrite
  dans [`compat.md`](compat.md) — le collecteur de tri **ramasse tous les
  documents correspondants** avec leurs clés avant de les ordonner, là où Lucene
  garde un tas de taille `size` alimenté par les *doc values*. La mesure donne
  enfin l'ordre de grandeur, et la loi : le coût est linéaire en nombre de
  documents **correspondants** (170 ms à 500 000, 727 ms à 2 000 000), pas en
  `size`.
- **L'indexation est cinq fois plus lente**, aux deux échelles : 11 298 doc/s
  contre 58 736, puis 10 198 contre 51 484. Recharger deux millions de documents
  prend trois minutes et quart contre trente-neuf secondes. C'est le chiffre le
  plus dur du banc après le tri, et c'est celui qui a demandé le plus de soin à
  mesurer : la décompression du corpus, laissée dans le chronomètre, l'avait
  d'abord fait passer pour ×0,28.
- **Une requête à 45 587 termes coûte huit fois plus cher** (1 023 ms contre
  126 ms à deux millions). C'est le prix d'un `terms` qui construit une union de
  45 587 sous-requêtes là où Lucene bascule sur un automate au-delà d'un seuil.
- **Le `scroll` est quatre fois plus lent** sur 25 pages de 1 000 documents
  (1 737 ms contre 433 ms). C'est le prix assumé du contexte figé : ferrite
  balaie **tout** le résultat à l'ouverture pour garantir « chaque document une
  fois, et une seule ». La première page paie donc pour les vingt-cinq.
- **`match_all` a changé de camp entre les deux échelles** : ×1,01 à cinq cent
  mille documents, ×0,67 à deux millions. L'explication qui vient à l'esprit —
  ferrite rend toujours un total exact quand Elasticsearch s'arrête à 10 000 —
  est **fausse**, et c'est la mesure qui le dit : le même `match_all` avec
  `track_total_hits: true` coûte 2,55 ms à ES contre 2,30 ms sans. Le coût vient
  d'ailleurs, et il grandit avec l'index (2,11 ms puis 5,00 ms) là où celui d'ES
  ne bouge pas.
- **La taille sur disque est 7 à 17 % plus grosse** une fois Elasticsearch
  fusionné — et ferrite n'a pas de `_forcemerge` pour aller chercher le même
  gain.

## La réserve qui comptait plus que les chiffres — et ce qu'elle est devenue

**Deux des treize requêtes ne rendaient pas la même chose des deux côtés**, aux
deux échelles : `country_agg_uncached` et `country_agg_cached`. Les `doc_count`
de chaque bucket étaient exacts ; c'étaient les **sous-agrégations** qui étaient
fausses. Sur 500 000 documents, `sum_population` du bucket `AE` valait 9 672 881
chez ferrite et 12 008 586 chez Elasticsearch — 19 % de la valeur manquante.

L'exemple le plus net était ailleurs, sur l'index de deux millions de documents,
et il tenait en une ligne : un `range` sur `population` dont le bucket
`100.0-*` compte **28 518 documents** rendait un `value_count` de **1 692**. Le
`doc_count` était juste, la sous-agrégation en avait perdu 94 %.

Le banc à l'échelle est ce qui l'a trouvé, et c'est sa vraie découverte. Le
défaut n'apparaissait qu'au-delà de 2 048 documents dans un même segment **et**
seulement dans les buckets **rares** — donc ni les 600 documents de
`bench_vs_es.py`, ni les 53 requêtes de
[`diff_aggs.py`](../tests/compat/diff_aggs.py), ni le fuzzer (25 documents par
cas) ne pouvaient le voir. Il était **silencieux** : la réponse est un 200 bien
formé, avec des `doc_count` justes.

### C'est corrigé, et voici ce qu'il a fallu mesurer pour le corriger

La cause était dans tantivy 0.26.1 (`aggregation/cached_sub_aggs.rs`,
`LowCardSubAggCache::flush_local`) : passé 2 048 documents en cache, il ne
recopiait que les buckets au-dessus d'un seuil, puis **effaçait le cache
entier** — les documents des buckets non recopiés étaient perdus.

Lire ça dans le code de la dépendance ne suffisait pas à décider. Les bornes ont
donc été **reproduites** contre un vrai Elasticsearch
([`sonde_sous_aggs.py --seuil`](../tests/compat/sonde_sous_aggs.py)), et deux
d'entre elles ont changé la gravité de ce qui était publié ici :

| | Ce qui était écrit | Ce que la mesure dit |
|---|---|---|
| Documents par segment | « au-delà de ~2 048 » | **2 047 juste, 2 048 faux** — le seuil est exact |
| Documents par bucket | « les buckets rares » | perdu si le bucket a **au plus `2048 / (2 × nombre de buckets)`** documents dans la fenêtre : 204 perdus, 205 gardés sur 5 buckets |
| Sous-agrégations touchées | `value_count`, un `sum` | **toutes** les métriques, **et** les sous-agrégations de buckets (`terms`, `range`, `histogram`) — 14 formes fausses sur 46 |
| La pire d'entre elles | — | `avg` rendait **21,5** au lieu de **21,428…** : un nombre faux *plausible*, celui qu'un tableau de bord affiche sans que personne ne sourcille |
| Parents épargnés | `histogram`, `terms` imbriqué | plus un `terms` à ≥ 100 valeurs et le `filter` qu'exécute ferrite — confirmé, ce n'est pas « les sous-agrégations sont approximatives », c'est un chemin précis |

Le défaut avait été signalé en amont pendant ce temps
([tantivy#2992](https://github.com/quickwit-oss/tantivy/issues/2992)) et corrigé
par le mainteneur, mais **non publié** : 0.26.1 reste la dernière version.
ferrite épingle donc ce correctif — le tag 0.26.1 plus ce seul commit, trois
lignes ajoutées et vingt-neuf retirées, dans un fork dont
[`verifie_tantivy.py`](../tests/compat/verifie_tantivy.py) montre qu'il est
identique aux neuf crates publiées à un fichier près. Ce que l'épingle contient
et comment en sortir : [`tantivy-patch.md`](tantivy-patch.md).

La mesure qui le tient ne demande plus 500 000 documents de `geonames` :
`sonde_sous_aggs.py` pose 46 combinaisons parent × sous-agrégation sur 50 000
documents déséquilibrés et les compare champ par champ à un vrai ES. **46/46
avec l'épingle, 32/46 sans** — c'est le second chiffre qui prouve que le premier
mesure quelque chose.

Le chiffre publié plus haut a quand même été rejoué sur le vrai corpus, parce
qu'une correction validée sur le cas réduit et jamais reposée sur le cas
d'origine reste une inférence. Deux millions de documents de la track,
réindexés dans les deux serveurs, la même agrégation posée aux deux :

```
   bucket *-100.0    doc_count=1971482   value_count ferrite=1971482   ES=1971482   ok
   bucket 100.0-*    doc_count=28518     value_count ferrite=28518     ES=28518     ok
   JSON identique : True
```

**28 518 documents, `value_count` de 28 518.** C'était 1 692.

### Ce que la correction coûte, et ce que les deux lignes du tableau valent encore

Les deux lignes `country_agg_*` des tableaux de latence ci-dessus ont été
mesurées par la campagne qui a trouvé le défaut, donc **sur un moteur qui ne
comptait pas tous les documents**. Elles disent le prix d'un calcul faux : à ce
titre le ×0,80 et le ×1,29 ne sont plus comparables à quoi que ce soit, et la
campagne n'a pas été relancée dans cette carte — deux échelles mesurées avec
deux moteurs ne se comparent pas plus que deux protocoles.

Le prix de la correction n'a pas pu être mesuré ici, et il vaut mieux le dire
que publier un chiffre qu'on ne tient pas : sur 500 000 documents, huit
agrégations, quatre tours alternés entre les deux binaires, les cas que le
correctif **ne touche pas** bougent de −11 % à +3 % d'un tour à l'autre — donc
plus que ceux qu'il touche. Le banc contrôlé du mainteneur de tantivy donne
+7,8 % à +11,6 % sur les agrégations concernées et des kilo-octets de mémoire.
C'est de toute façon la mauvaise question : la version rapide rendait un
`value_count` de 1 692 sur 28 518 documents.

## Jusqu'où ferrite est le bon choix

Ce qui suit ne sort que de ce qui a été mesuré ci-dessus. Rien n'a été mesuré
au-delà de deux millions de documents, et rien n'est extrapolé.

**Le bon choix**, jusqu'à quelques millions de documents, quand la charge est
faite de recherches filtrées qui ramènent peu : c'est là que ferrite est devant
sur la latence — et son avance grandit avec la taille — et surtout à un ordre de
grandeur de mémoire en dessous. Un sidecar, un conteneur de CI, un environnement
de développement, une application mono-nœud : le déploiement redevient un détail
sans que la recherche coûte plus cher.

**Le mauvais choix**, dès qu'une de ces trois lignes décrit la charge :

1. **le tri sur de gros résultats.** Le coût est linéaire en nombre de documents
   correspondants. Au-delà de quelques centaines de milliers de correspondances
   par requête, un tri passe la demi-seconde quand Elasticsearch reste sous les
   dix millisecondes. Une liste triée paginée sur un index de plusieurs millions
   de documents n'est pas pour ferrite ;
2. **l'export massif et répété.** Un `scroll` de 25 000 documents coûte 1,7 s.
   Une sauvegarde nocturne, oui ; un export à la demande dans une boucle, non ;
3. **le réindexage fréquent.** À ~10 000 doc/s, remplir dix millions de
   documents prendrait un quart d'heure, contre trois minutes à Elasticsearch —
   et le débit d'indexation de ferrite ne monte pas avec la machine comme le
   sien.

L'avertissement qui figurait ici — « une facette calculée sur plus de quelques
milliers de documents rend des valeurs fausses dans ses buckets rares » — est
levé : c'était la limite la plus dure de cette page, elle est corrigée, et la
mesure qui l'a établie est aussi celle qui la tient
([`sonde_sous_aggs.py`](../tests/compat/sonde_sous_aggs.py), 46/46 contre un
vrai Elasticsearch, 32/46 sans le correctif).

## Refaire la mesure

```bash
docker run -d --name es-bench -p 9201:9200 \
  -e discovery.type=single-node -e xpack.security.enabled=false \
  -e ES_JAVA_OPTS="-Xms2g -Xmx2g" \
  docker.elastic.co/elasticsearch/elasticsearch:8.15.0

docker build -t ferrite:bench .
mkdir -p .bench-echelle/data-ferrite
docker run -d --name ferrite-bench -p 9200:9200 \
  -v "$PWD/.bench-echelle/data-ferrite:/data" ferrite:bench

python3 tests/compat/bench_echelle.py --inventaire      # ce que la track demande
python3 tests/compat/bench_echelle.py --docs 500000  --json docs/bench.json \
  --ferrite-conteneur ferrite-bench --es-conteneur es-bench
python3 tests/compat/bench_echelle.py --docs 2000000 --json docs/bench.json \
  --ferrite-conteneur ferrite-bench --es-conteneur es-bench
```

Le corpus (265 Mo compressés) est téléchargé une fois dans `.bench-echelle/` et
sa taille est vérifiée à l'octet près contre celle que `track.json` déclare.
Sans `--ferrite-conteneur` / `--es-conteneur`, les lignes RSS sortent
**« non mesuré »** plutôt que zéro : une valeur par défaut plausible est le
déguisement le plus efficace d'un chiffre absent.

## Ce que le banc a coûté avant de rien mesurer

Trois campagnes complètes, parce que deux défauts du **protocole** ont été
trouvés après coup — et les deux allaient dans le sens de flatter ferrite :

1. le `_forcemerge` d'Elasticsearch tournait **avant** ses propres chronomètres,
   donc ses fusions le ralentissaient pendant sa mesure de latence. Il est passé
   après, et on attend maintenant la fin des fusions comme le fait la track ;
2. la décompression du corpus était consommée **pendant** l'indexation, donc les
   deux temps d'indexation portaient une constante commune qui écrasait l'écart.
   Le corpus est préparé avant le chronomètre. L'indexation d'Elasticsearch est
   passée de 38 116 à 58 736 doc/s ; celle de ferrite, de 10 584 à 11 298.

C'est le geste 2 de [`CLAUDE.md`](../CLAUDE.md) appliqué à soi-même : étalonner
l'instrument avant de mesurer. Deux échelles mesurées avec deux protocoles ne se
comparent pas — la campagne a été relancée en entier à chaque fois.
