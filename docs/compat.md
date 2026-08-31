# compat — ce que ferrite sait faire, et ce qu'il refuse

> **Ce fichier est généré.** Sa source est [`compat.yaml`](../compat.yaml) à la
> racine — une entrée par capacité, avec son état, ses paramètres et, pour un
> refus, son motif. Le texte long, lui, est écrit à la main dans
> [`compat.gabarit.md`](compat.gabarit.md), où un marqueur dit où va chaque
> table. Pour regénérer les deux fichiers dérivés :
> `python3 tests/compat/genere_compat.py` — la CI échoue si le résultat diffère
> de ce qui est commité.

Inventaire du support de l'API Elasticsearch par ferrite. **Mis à jour dans la
PR qui change le comportement, pas après.**

| | |
|---|---|
| ✅ | supporté, vérifié par le harnais de compat (`tests/compat/`) |
| 🟡 | partiel — la partie supportée est décrite, le reste est refusé |
| ❌ | refusé **explicitement**, avec une erreur au format d'Elasticsearch |

Un ❌ porte toujours son **motif**, parce que « je ne sais pas encore faire » et
« je refuse exprès » ne se corrigent pas de la même façon :

| Motif | Ce qu'il veut dire |
|---|---|
| **hors périmètre assumé** | ferrite ne le fera pas : c'est ce qui fait le coût d'Elasticsearch, et ce dont un déploiement mono-conteneur n'a pas besoin |
| **pas encore** | rien ne s'y oppose, ce n'est pas écrit. Un manque, pas une impossibilité |
| **divergence de moteur** | tantivy ne fait pas exactement ce que fait Lucene ; l'accepter quand même rendrait d'autres résultats qu'Elasticsearch, en silence |
| **comme Elasticsearch** | un refus qui **est** la compatibilité : Elasticsearch refuse aussi, et ferrite reproduit son erreur |

La règle qui prime : **jamais d'échec silencieux**. Rien de ce qui figure en ❌
ne renvoie « 0 résultat » ou un résultat partiel — tout produit une erreur
lisible. Le type d'erreur `not_implemented_in_ferrite_exception` signale
précisément « Elasticsearch sait faire, ferrite pas encore ».

Version d'API annoncée : **Elasticsearch 8.15.0** (`version.number`,
`_nodes`). Toutes les réponses portent `X-elastic-product: Elasticsearch`.

**Les suites de conformance de deux autres moteurs** (`tests/compat/conformance_es.py`)
donnent l'état d'ensemble : celle d'Elasticsearch 7.10.2 — la dernière version
Apache 2.0 — et celle d'**OpenSearch 2.19.1**, Apache 2.0 elle aussi, descendante
du même fork de 2020 mais maintenue depuis. Leurs chiffres ne sont pas recopiés
ici : ils vivent dans [`conformance.json`](conformance.json) et
[`conformance-opensearch.json`](conformance-opensearch.json), régénérés par le
runner et commités (les étalonnages contre un vrai serveur de chaque moteur sont
dans [`conformance-es7102.json`](conformance-es7102.json) et
[`conformance-opensearch-os2191.json`](conformance-opensearch-os2191.json)).

```bash
python3 -c "import json; print(json.load(open('docs/conformance.json'))['totaux'])"
python3 -c "import json; print(json.load(open('docs/conformance-opensearch.json'))['totaux'])"
```

La colonne « échecs » de ces rapports est l'inventaire des écarts qui restent —
les familles sont listées dans [`conformance.md`](conformance.md), avec de quoi
les compter soi-même. C'est la mesure la moins complaisante du projet : les cas
viennent d'Elastic et d'OpenSearch, pas de nous, et une seule des deux sources
serait un examen dont on connaît le sujet. La CI en fait un cliquet, sur les
deux : le nombre d'échecs ne remonte pas.

Sur la suite d'OpenSearch, un troisième verdict existe et il est **mesuré** :
un cas qu'un **vrai Elasticsearch 8.15** échoue lui aussi ne mesure pas ferrite,
il mesure ce sur quoi les deux moteurs ne s'accordent plus. La référence est
[`conformance-opensearch-es8150.json`](conformance-opensearch-es8150.json).

**Le fuzzing différentiel** ([`fuzz.md`](fuzz.md)) mesure ce qui reste en dehors
de ces deux inventaires : mapping, documents et requêtes tirés au sort **dans le
périmètre déclaré par cette page**, posés à ferrite et à un vrai Elasticsearch
8.15, réponses comparées champ par champ. La mesure du jour est dans
[`fuzz.json`](fuzz.json). Les divergences qu'il laisse passer sont celles que
cette page déclare — chacune porte un prédicat écrit dans l'outil, pas un code
d'état toléré en bloc.

### Ce que le corpus d'usage réclame le plus

Ce tableau-là ne dit pas ce que ferrite tient, il dit **ce qu'on lui demande**.
Chaque capacité porte un `poids` : la part des requêtes d'un corpus de vraies
requêtes — documentation de référence d'ES 8.15, tracks Rally d'Elastic, tests
et exemples des clients officiels, code open source — qui l'exercent. La
méthode, les sources et leurs biais sont dans [`usage.md`](usage.md) ; les
nombres viennent de [`usage.json`](usage.json), et `ponderation.py --verifie`
échoue si `compat.yaml` s'en écarte. Un poids n'est jamais écrit à la main :
une capacité qu'aucune requête du corpus ne sait exercer garde `null` plutôt
qu'un zéro qui aurait l'air d'une mesure.

| Capacité | Part des requêtes du corpus | État |
|---|---|---|
| `POST\|GET /{index}/_search`, `POST\|GET /_search` | 33,9 % | ✅ |
| `query` | 25,3 % | ✅ |
| Tout ce qu'Elastic ajoute autour du moteur — sécurité et rôles (`_security`), machine learning (`_ml`), cycle de vie (`_ilm`, `_slm`), `_watcher`, `_transform`, `_enrich`, `_eql`, `_sql`, `_esql`, `_inference`, connecteurs, *search applications*, `_rollup`, `_ccr`, `_graph`, licence, `_monitoring` | 18,9 % | ❌ |
| `from` / `size` | 16,3 % | ✅ |
| `aggs` / `aggregations` | 14,0 % | 🟡 |
| `bool` | 12,1 % | 🟡 |
| `range` | 11,4 % | 🟡 |
| `PUT /{index}` | 10,5 % | 🟡 |
| Sous-agrégations | 8,8 % | ✅ |
| `terms` | 7,5 % | 🟡 |
| `match` | 6,8 % | 🟡 |
| `PUT\|POST /{index}/_doc/{id}` | 6,8 % | 🟡 |
| `stored_fields` | 6,6 % | ✅ |
| `percentiles`, `extended_stats`, `top_hits`, `composite`, `filters`, `nested`, `significant_terms`, `date_range`, `ip_range`… | 6,1 % | ❌ |
| `query_string`, `simple_query_string`, `intervals`, `terms_set`, `script`… | 6,1 % | ❌ |
| `date_histogram` | 5,3 % | 🟡 |
| `match_phrase` | 4,5 % | 🟡 |
| `_all`, `*`, URL sans index | 4,1 % | ✅ |
| `_source` | 3,7 % | ✅ |
| `min`, `max`, `sum`, `avg`, `value_count`, `stats` | 3,6 % | ✅ |

---

## Poignée de main et cluster

| Route | État | Détail |
|---|---|---|
| `GET /` | ✅ | `name`, `cluster_name`, `cluster_uuid`, bloc `version` complet, `tagline` |
| `HEAD /` (`ping`) | ✅ | |
| `GET /_cluster/health`, `/_cluster/health/{index}` | 🟡 | toujours `green`, 1 nœud, 0 shard non assigné. Supporté : `wait_for_status` (accepté et sans objet, déjà vert), `timeout` (accepté et sans objet, déjà vert). Refusé : `level` (il change la forme de la réponse), `wait_for_active_shards`, `wait_for_nodes`, `wait_for_events`, `wait_for_no_relocating_shards`, `wait_for_no_initializing_shards`, `local` (il n'y a qu'un nœud, donc rien de local à opposer au maître) |
| `GET /_cat/indices`, `/_cat/indices/{index}` | 🟡 | texte aligné par défaut. Supporté : `format=json`, `v`. Refusé : `h`, `s`, `bytes`, `help` |
| `GET /_cat/health` | 🟡 | idem. Supporté : `format=json`, `v`. Refusé : `h`, `s`, `help`, `ts` |
| `GET /_nodes`, `/_nodes/{spec}` | 🟡 | un nœud, champs d'identité et `http` ; pas de `settings`, `os`, `jvm`. `{spec}` accepte `_all`, `_local`, `_master` et l'identifiant du nœud. Refusé : les sous-ressources (`_nodes/stats`, `_nodes/hot_threads`…), `/_nodes/{spec}/{metric}` (donc le *sniffing*, qui demande `/_nodes/_all/http` : refusé en le nommant, et les trois clients officiels restent utilisables après ce refus (mesuré)) |
| `Content-Encoding: gzip` / `deflate` sur le corps d'une requête | ✅ | ce que pose tout client officiel à qui on demande de compresser — `http_compress=True` en Python, `compression: true` en JavaScript (activé **par défaut** vers Elastic Cloud), `CompressRequestBody` en Go. `gzip` et `deflate` sont décompressés ; `br` et un encodage inconnu sont transmis tels quels, exactement comme le fait Elasticsearch — mesuré, les deux. Un corps annoncé compressé et illisible est refusé **en nommant l'encodage**, là où ES rend « request body is required », qui désigne la mauvaise cause |

## Hors périmètre déclaré

Les familles de routes qu'Elasticsearch a et que ferrite n'a pas. Elles étaient
jusqu'ici décrites en une phrase du README (« sharding, réplication, consensus…
Painless ») : elles sont désormais **déclarées**, une famille à la fois, avec
son motif. C'est ce qui permet au rapport de conformance de trancher — un cas
qui échoue sur `_snapshot` n'est pas le même événement qu'un cas qui échoue sur
`_search`.

| Famille de routes | État | Pourquoi |
|---|---|---|
| Cluster distribué — `_cluster/state`, `_cluster/stats`, `_cluster/reroute`, `_cluster/allocation/explain`, `_cluster/pending_tasks`, `_remote/info`, `_nodes/stats`, `_tasks` | ❌ | **hors périmètre assumé** — ferrite est mono-nœud et mono-shard : il n'y a ni allocation, ni réallocation, ni tâche distribuée à rapporter, et une réponse plausible inventée pour ces routes serait un mensonge sur la nature du serveur |
| Les `_cat/*` autres que `_cat/indices` et `_cat/health` | ❌ | **pas encore** — ce sont des vues de texte sur un état que ferrite a pour la plupart ; elles ne sont pas écrites, et leurs tests exigent en plus les colonnes `h`, `s` et `help` que les deux `_cat` existants n'ont pas non plus |
| Snapshots, dépôts, restauration — `_snapshot/*` | ❌ | **hors périmètre assumé** — la sauvegarde d'un ferrite est la copie de son répertoire de données, ou l'export par `scroll` ; un dépôt de snapshots répliqué est de la mécanique de cluster |
| Flux de données — `_data_stream`, `data_stream` dans un template | ❌ | **hors périmètre assumé** — un flux de données est une suite d'index gérée par `_rollover` et l'ILM, donc de l'exploitation de cluster posée au-dessus du moteur. Le refus est déclaré ici parce qu'il est **exercé** : `helpers.reindex` du client officiel demande `GET /_data_stream/{cible}` avant de copier, sur toute cible — c'est la suite serveur du client Python qui l'a montré, et sans cette ligne l'appel comptait `indeterminé`, donc contre nous |
| Cycle de vie d'un index — `_close`, `_open`, `_forcemerge`, `_shrink`, `_split`, `_clone`, `_rollover`, `_recovery`, `_segments`, `_flush`, `_upgrade`, `_cache/clear`, `_shard_stores`, `_resolve/index` | ❌ | **pas encore** — la moitié de ces routes n'a pas de sens sans shards (`_shrink`, `_split`, `_recovery`, `_shard_stores`) ; l'autre moitié suppose un index qu'on peut arrêter de servir sans le supprimer (`_close` / `_open`), état que ferrite n'a pas |
| Les autres routes de recherche — `_search_shards`, `_termvectors`, `_mtermvectors`, `_suggest`, `_source` | ❌ | **pas encore** — aucune n'est demandée par le code client qu'on cherche à servir |
| L'API typée — un `{type}` dans l'URL, un `_type` dans la réponse, `include_type_name` | ❌ | **comme Elasticsearch** — elle a disparu en 8.x, la version que ferrite annonce : un vrai Elasticsearch 8 échoue au même endroit, et la rendre reviendrait à annoncer une version qu'on ne sert pas |
| Tout ce qu'Elastic ajoute autour du moteur — sécurité et rôles (`_security`), machine learning (`_ml`), cycle de vie (`_ilm`, `_slm`), `_watcher`, `_transform`, `_enrich`, `_eql`, `_sql`, `_esql`, `_inference`, connecteurs, *search applications*, `_rollup`, `_ccr`, `_graph`, licence, `_monitoring` | ❌ | **hors périmètre assumé** — ce sont des produits posés sur le moteur, pas le moteur : chacun a son propre modèle de données et sa propre API, et aucun n'est ce qu'un client Elasticsearch existant appelle pour chercher des documents. C'est le corpus d'usage qui a montré qu'aucune capacité ne les réclamait — donc que **19 % de ses requêtes** tombaient dans un trou de la déclaration, ni servies ni refusées |
| Scripting — `_scripts`, `_search/template`, `_render/template`, Painless | ❌ | **hors périmètre assumé** — un moteur de script est un langage à embarquer, à isoler et à maintenir : c'est exactement le genre de poids que ferrite existe pour ne pas porter |

C'est de loin la famille d'écarts la plus fournie de la suite de conformance
d'Elastic : l'écrasante majorité de ses échecs est un `no handler found for uri
[...]`, c'est-à-dire une route qu'ES a et que ferrite n'a pas. Le compte du jour
se lit dans [`conformance.json`](conformance.json), qui range désormais chaque
échec en **régression** (une capacité déclarée supportée) ou en **coût de
périmètre** (une capacité déclarée refusée).

## Index et mapping

| | État | Détail |
|---|---|---|
| Création à l'écriture | ✅ | indexer dans un index absent le **crée**, comme ES (`action.auto_create_index`) : `index`, `create`, `update`, et le `_bulk`. La lecture, la recherche et la suppression rendent toujours 404 |
| Expressions d'index (`a,b`, `logs-*`, `_all`, exclusions, alias) | ✅ | sur **toutes** les routes, recherche comprise — voir la section dédiée |
| `PUT /{index}` | 🟡 | `mappings` est **optionnel** (les champs viendront des documents) ; `settings` s'écrit à plat comme imbriqué. Supporté : `mappings`, `aliases`, `settings.number_of_shards` (accepté, sans effet : ferrite est mono-shard), `settings.number_of_replicas` (accepté, sans effet), `settings.index.query.parse.allow_unmapped_fields` (voir plus bas), `settings.index.max_ngram_diff` (voir plus bas), `settings.analysis` (`analyzer`, `tokenizer`, `filter` — voir la section des analyzers sur mesure). Refusé : tout autre réglage (refusé plutôt qu'ignoré) |
| `DELETE /{index}` | ✅ | listes et motifs, sous `action.destructive_requires_name` (voir plus bas). `ignore_unavailable` honoré |
| `HEAD /{index}` | ✅ | 200 dès que l'expression se résout, même sur zéro index — comme ES |
| `GET /{index}` | ✅ | `aliases` / `mappings` / `settings`, une entrée par index visé |
| `GET /{index}/_mapping` | ✅ | |
| `GET /{index}/_mapping/field/{champs}` | ❌ | **pas encore** — route absente (`no handler found`) : ferrite a pourtant le mapping, c'est un manque et pas une impossibilité — 15 cas de la suite d'Elastic tombent dessus |
| `GET /{index}/_settings` | 🟡 | les réglages d'ES qu'un index a vraiment (`number_of_shards`, `uuid`, `creation_date`…), et `index.query.parse.allow_unmapped_fields` s'il a été posé. Supporté : `GET /_settings` sans index (vaut `_all`), `/{index}/_settings/{nom}` (filtrer par nom de réglage — liste, jokers, `_all`. Le filtre porte sur les clés **aplaties**, sans quoi le même nom filtrerait autrement selon `flat_settings`), `flat_settings` (il aplatit les clés (`index.number_of_shards`) ; longtemps refusé, parce qu'accepté et ignoré il rendait une réponse que personne n'avait demandée — c'est une réécriture de clés, elle est maintenant faite), `local` (un seul nœud : la question ne se pose pas). Refusé : `include_defaults` (il ajoute une section `defaults` avec les dizaines de réglages qu'ES a et que ferrite n'a pas), la section `analysis` (ES rend la déclaration d'analyse de l'index ; ferrite ne la rend pas. Il n'en garde que la forme **normalisée** dont il a besoin pour rejouer les analyzers, et rendre celle-là ferait lire au client des noms de filtres qu'il n'a jamais écrits — ce qui serait pire qu'une absence. Les analyzers eux-mêmes sont bien là : `GET /{index}/_mapping` rend le nom déclaré sur chaque champ, et `_analyze` les exerce) |
| `PUT /{index}/_settings` | 🟡 | les réglages **inertes** sont acceptés, gardés et rendus par `GET /{index}/_settings` : ils décrivent déjà ce que ferrite est (mono-shard, sans réplique). Faire échouer un script d'init entier sur un `number_of_replicas: 1` qui ne changerait rien serait pire que de l'accepter. `index.refresh_interval` n'est pas accepté-et-ignoré : `-1` sort vraiment l'index de la boucle de rafraîchissement de fond. Supporté : `number_of_replicas` (sans effet — ferrite n'a pas de réplique), `auto_expand_replicas` (sans effet, même raison), `refresh_interval` (`-1` désactive vraiment le rafraîchissement de fond ; une valeur positive est honorée, ferrite rafraîchissant toutes les secondes), `preserve_existing`, `max_ngram_diff` (l'écart maximal entre `max_gram` et `min_gram` d'un `ngram` (défaut 1) ; il **valide** ce que `settings.analysis` déclare, c'est le seul réglage d'index dans ce cas), une valeur `null` (efface le réglage, comme chez ES). Refusé : `number_of_shards` (figé à la création, comme chez ES (`Can't update non dynamic settings`)), `index.query.parse.allow_unmapped_fields` (figé dans la génération courante du schéma ; un client qui le croirait changé chercherait longtemps), `reopen`, tout autre réglage d'ES (`index.blocks.*`, `index.max_result_window`… : les accepter puis les ignorer changerait le comportement en silence) |
| `PUT /{index}/_mapping` | 🟡 | **ajoute** des champs (une nouvelle génération est construite). Changer le type d'un champ existant reste refusé, comme chez ES. Refusé : `dynamic` (le modifier après coup) |
| `POST /{index}/_refresh` | ✅ | |
| `POST\|GET /_analyze`, `/{index}/_analyze` | 🟡 | les offsets sont comptés en **unités UTF-16**, comme chez ES (c'est la convention de Java) ; sur plusieurs `text`, chaque texte suivant décale les offsets de la longueur du précédent **plus un**, et les positions de 100 quand l'analyzer est sur mesure, de 0 quand il est intégré. Supporté : `text` (chaîne ou liste), `analyzer`, `field`, `tokenizer` (un nom (`ngram`) ou un objet (`{"type": "ngram", …}`), sans index — c'est ce qui rend une brique d'analyse mesurable toute seule), `filter` (une liste de noms ou d'objets, même règle). Refusé : `char_filter`, `normalizer`, `explain`, `attributes`, le champ `type` du token (ferrite rend toujours `<ALPHANUM>`, là où ES distingue `<NUM>`, `word`, `<EMOJI>`… ; il ne change ni les termes indexés ni les résultats) |
| Mapping dynamique | ✅ | `dynamic` : `true` (défaut), `false`, `strict`. `runtime` ❌. Voir plus bas |
| Alias | ✅ | voir la section dédiée |
| Templates d'index — `_index_template` et `_template` | 🟡 | `PUT` / `GET` / `HEAD` / `DELETE` sur les deux familles, et l'application du template à la création de l'index — **implicite** (une écriture dans un index absent) comme **explicite** (`PUT /{index}`, où le corps l'emporte). Un composable qui correspond éclipse les anciens ; sinon tous les anciens qui correspondent sont fusionnés par `order` croissant, comme chez ES. Le contenu est validé **à la pose** : un réglage refusé, un type de champ inconnu ou un alias filtré font échouer le `PUT`, là où le client regarde. Supporté : `index_patterns`, `template` (`settings`, `mappings`, `aliases`), `priority` (composables — le plus fort gagne, et deux motifs qui se recouvrent à priorité égale sont refusés comme chez ES), `order` (anciens — fusion par ordre croissant), `version`, `_meta`, `create`, `flat_settings` (sur `GET /_template`). Refusé : `composed_of` (les templates de composants (`_component_template`) ne sont pas implémentés ; appliquer un template qui en cite un sans le lire donnerait un index sans le mapping demandé), `data_stream`, `include_defaults` |
| `_component_template`, `_simulate_index_template`, `_simulate` | ❌ | **pas encore** — un template de composants est un template qu'on cite depuis un autre : tant qu'il n'est pas lu, `composed_of` est refusé à la pose plutôt qu'appliqué à moitié. La simulation, elle, rend l'index qu'on obtiendrait — utile, et sans client qui la demande |
| `GET /{index}/_stats` | 🟡 | la forme d'ES — `_shards`, `_all` (`primaries` / `total`), `indices` — et les quatre groupes que ferrite **mesure**. Sur un moteur mono-shard sans réplique, `primaries` et `total` portent les mêmes nombres : c'est vrai, pas une simplification. Supporté : `docs` (`count`, `deleted`), `store` (`size_in_bytes`, `total_data_set_size_in_bytes`), `segments` (`count`), `shard_stats` (`total_count`), `level` (`cluster`, `indices`, `shards`). Refusé : les autres groupes de compteurs (`indexing`, `search`, `get`, `merge`, `translog`, les caches… : ferrite ne les tient pas, et un `index_total: 0` sur un index qu'on vient de remplir ferait passer « non mesuré » pour « aucune activité »), `fields`, `groups`, `completion_fields`, `fielddata_fields`, `_shards.total` (un shard par index, toujours : un cas de la suite d'Elastic qui crée un index à 5 shards et en attend 10 au total ne peut pas passer ici — et ne devrait pas) |

### Mapping dynamique

`dynamic` vaut `true` par défaut, comme chez ES.

| Valeur | Comportement |
|---|---|
| `true` | le type du champ est deviné et le mapping grandit |
| `false` | le champ reste dans `_source`, sans être indexé ni interrogeable |
| `strict` | le document est refusé (`strict_dynamic_mapping_exception`) |
| `runtime` | ❌ |

Les règles d'inférence sont celles d'ES, vérifiées champ par champ contre un
vrai 8.15 : chaîne → `text` **plus un sous-champ `.keyword`** (`ignore_above:
256`), entier → `long`, flottant → `float`, booléen → `boolean`, chaîne de date
ISO → `date`. `numeric_detection` est désactivé comme chez ES, donc `"42"` reste
du texte. Un tableau prend le type de son premier élément non nul ; `null` et le
tableau vide ne créent pas de champ.

**Ce que ça coûte.** tantivy fige le schéma à la création de l'index : ferrite ne
peut pas y ajouter un champ. Quand le mapping dynamique en découvre un, ferrite
construit donc une **nouvelle génération** de l'index et y rejoue tous les
documents depuis le `_source` qu'il conserve déjà. Mesuré sur ce worker :

| Documents déjà indexés | Durée de l'ajout d'un champ |
|---|---|
| 100 | 23 ms |
| 1 000 | 29 ms |
| 10 000 | 92 ms |
| 50 000 | 446 ms |

Soit environ 9 µs par document, linéaire. En pratique les nouveaux champs
apparaissent au début de la vie d'un index, quand il est encore petit. Un champ
qui apparaît après un million de documents coûterait en revanche plusieurs
secondes — c'est la contrepartie assumée d'un schéma figé, et `dynamic: strict`
reste là pour l'éviter.

La bascule est sûre : la nouvelle génération est entièrement écrite et validée
avant que `ferrite.json` ne la désigne (écriture atomique par renommage), et une
écriture en cours empêche la bascule le temps qu'elle se termine. Les générations
remplacées ne sont effacées que lorsque plus aucune recherche ne les tient.

### Analyzers

Chaque analyzer intégré est comparé **token par token** à son homonyme d'ES sur
**217 textes** français et anglais (`tests/compat/diff_analyzers.py`, 51
batteries, toutes identiques) : des
phrases, un vocabulaire qui balaie les familles de suffixes, et des mots plus
longs que la limite de 255 caractères des tokenizers de Lucene — qui la
**coupent** au lieu de jeter le mot, et décalent d'autant les positions de tout
ce qui suit.

| Analyzer | État | Détail |
|---|---|---|
| `standard` (défaut) | ✅ | identique à ES sur les 217 textes |
| `simple` | ✅ | identique |
| `whitespace` | ✅ | identique |
| `keyword` | ✅ | identique |
| `stop` | ✅ | identique |
| `english` | ✅ | identique — Porter porté depuis Lucene, filtre possessif compris |
| `french` | ✅ | identique — stemmer léger de Savoy, élision, mots vides relevés |
| `snowball` | ✅ | identique — `standard` + minuscules + mots vides anglais + Snowball **anglais** (`porter2`), qui n'est pas le Porter de `english` : `quickly` rend `quick` ici et `quickli` là |
| `danish`, `dutch`, `german`, `hungarian`, `italian`, `norwegian`, `portuguese`, `romanian`, `russian`, `spanish`, `swedish`, `turkish` | ✅ | identiques à ES sur les vocabulaires du projet Snowball (BSD-3-Clause, 20 913 à 96 325 mots par langue, écrits par d'autres) — **positions et offsets compris**, `tests/compat/sonde_langues.py`. Le refus reposait sur « le stemmer de tantivy n'est pas celui de Lucene » : la mesure a montré que c'est faux pour 8 de ces 12 langues (0 écart), et que le reste tenait aux mots vides, à quatre filtres absents et aux stemmers **légers** que l'allemand, l'espagnol, l'italien et le portugais d'ES posent à la place de Snowball |
| `finnish` | ❌ | **divergence de moteur** — le stemmer finnois de `rust-stemmers` s'écarte de celui de Lucene sur **13 mots des 84 399** du vocabulaire finnois de Snowball (0,015 %) : il coupe la voyelle finale d'un emprunt à diacritique étranger là où l'algorithme la garde (`garcía` rend `garcí` chez lui et `garcía` chez ES ; de même `bundesstraße`, `españa`, `musée`). Un analyzer n'est jamais livré sous le nom d'ES tant qu'il n'est pas mesuré identique — un écart de 0,015 % rendu en 200 est le pire résultat possible ici |
| `arabic`, `czech`, `greek`, `thai` et les autres langues | ❌ | **pas encore** — leur chaîne demande des filtres de normalisation qui ne sont pas portés (et, pour plusieurs, un stemmer absent de `rust-stemmers`). Rien n'y est mesuré : les douze langues servies le sont mot à mot, et celles-ci ne le seraient pas |
| Analyzers sur mesure (`settings.analysis`) | ✅ | voir ci-dessous |

**Les stemmers de Lucene sont portés** (`src/stemmer.rs`) : le stemmer Porter
pour l'anglais, le stemmer léger de Savoy pour le français. Celui de tantivy
(Snowball) n'est celui d'aucun des deux — c'est ce qui donnait, avant ce
portage, **19 textes divergents sur 28 en `english` et 17 en `french`**.

**Les deux sont désormais identiques à ES sur les 217 textes.** `english` :
Porter (validé en plus sur les 66 exemples de l'article de Porter lui-même),
filtre possessif (`Peter's` → `Peter`), mots vides et ordre des filtres de
`EnglishAnalyzer`. `french` : stemmer léger de Savoy, élision (`l'ascension` →
`ascension`), et une liste de mots vides **relevée mot à mot** sur un vrai ES
(`tests/compat/releve_mots_vides.py`) — elle n'est ni celle de Snowball (qui
garde `est`) ni l'ancienne de Lucene (elle retire `ceci`, `cette`, `avec`,
`sans`, `ils`), donc la deviner n'était pas une option.

#### Les analyzers de langue : d'où venait l'écart

Ils étaient refusés en bloc, sur une raison qui paraissait solide : « le stemmer
de tantivy (Snowball) n'est pas celui de Lucene ». **Elle est fausse pour la
plupart des langues**, et c'est une mesure qui l'a dit.

Le corpus n'est pas le nôtre : ce sont les vocabulaires du projet **Snowball**
(BSD-3-Clause, licence vérifiée dans le dépôt), 20 913 à 96 325 mots par langue,
**563 000 mots** en tout. Le tableau ci-dessous est le cœur de la mesure : pour
chaque langue, le nombre de mots dont l'analyse **diffère de celle de
l'analyzer nommé d'ES**, la chaîne étant arrêtée après chaque étape. La colonne
« minuscules » est ce que ferrite savait faire avant — `standard` tout seul.

| Langue | mots | minuscules | + élision / apostrophe | + mots vides | + normalisation | + stemmer |
|---|---:|---:|---:|---:|---:|---:|
| `danish` | 23 868 | 16 676 | 16 676 | 16 582 | 16 582 | **0** |
| `dutch` | 45 670 | 24 952 | 24 952 | 24 851 | 24 851 | **0** |
| `german` | 35 053 | 24 143 | 24 143 | 23 917 | 21 333 | **0** |
| `hungarian` | 29 881 | 25 574 | 25 574 | 25 503 | 25 503 | **0** |
| `italian` | 37 426 | 30 018 | 29 612 | 29 323 | 29 323 | **0** |
| `norwegian` | 20 913 | 14 438 | 14 438 | 14 297 | 14 297 | **0** |
| `portuguese` | 32 016 | 23 007 | 23 007 | 22 834 | 22 834 | **0** |
| `romanian` | 87 642 | 76 318 | 76 318 | 76 092 | 76 092 | **0** |
| `russian` | 49 785 | 45 870 | 45 870 | 45 711 | 45 711 | **0** |
| `spanish` | 28 378 | 21 465 | 21 465 | 21 247 | 21 247 | **0** |
| `swedish` | 30 738 | 20 890 | 20 890 | 20 776 | 20 776 | **0** |
| `turkish` | 96 325 | 71 167 | 67 105 | 66 807 | 66 807 | **0** |
| `finnish` | 84 399 | 70 288 | 70 288 | 70 115 | 70 115 | **0** |

Trois choses s'y lisent, et la troisième renverse le pari de départ.

- **L'écart brut est énorme** : sans analyzer de langue, 68 % à 87 % des mots
  d'un vocabulaire s'indexent autrement que chez ES. C'est la marche la plus
  haute qui restait pour tout contenu non anglais ni français.
- **Les mots vides n'en expliquent presque rien** — 0,3 % à 0,8 %. C'est
  attendu sur un *vocabulaire* (un mot vide y compte une fois) et ce serait tout
  autre chose sur du texte suivi ; mais pour ce que l'index contient, le
  stemmer fait le travail.
- **Le stemmer, lui, ferme tout.** Et il n'a pas fallu l'écrire : sur huit de
  ces langues, le Snowball de tantivy est **identique à l'octet** à celui de
  Lucene — 0 écart sur 45 670 mots néerlandais, 96 325 turcs, 87 642 roumains.

Ce qui manquait n'était donc pas l'algorithme, mais quatre choses autour :

1. **Les listes de mots vides.** Elles sont désormais **lues dans le
   `lucene-analysis-common-*.jar` du conteneur de référence** — le fichier
   qu'Elasticsearch ouvre lui-même — puis vérifiées contre lui dans les deux
   sens : chaque mot de la liste doit ne rendre aucun token, et sur le
   vocabulaire complet aucun mot hors liste ne doit disparaître. Le relevé
   par candidats qui servait au français avait manqué `celà` : un mot qu'ES
   écarte et que ferrite indexait, en silence.
2. **Quatre filtres absents de tantivy** : la normalisation allemande (un
   automate à trois états — `haeuser` perd son `e`, `quelle` le garde),
   l'élision italienne, l'apostrophe turque (`Diyarbakır'ın` → `Diyarbakır`) et
   les minuscules turques (`ISTANBUL` → `ıstanbul`, pas `istanbul`).
3. **Quatre stemmers légers.** L'allemand, l'espagnol, l'italien et le portugais
   d'ES **n'emploient pas Snowball** mais les stemmers *légers* de Savoy — la
   même famille que le `french_light` déjà porté. Mesure : sur l'allemand, la
   chaîne bâtie avec Snowball s'écarte de l'analyzer `german` sur 445 mots,
   celle bâtie avec le stemmer léger sur **0**.
4. **Deux règles qui vivent dans le stemmer**, qu'aucun échantillon n'aurait
   sorties. Le `prélude` de l'algorithme russe (ё → е partout dans le mot) que
   `rust-stemmers` n'applique pas : 112 écarts sur 49 785 sans lui, 0 avec. Et
   le dictionnaire de **quatre mots** que `DutchAnalyzer` impose avant son
   stemmer (`ei` → `eier`, `kind` → `kinder`, `fiets` et `bromfiets` intacts) :
   quatre sur 45 670, donc invisibles sur les 3 000 mots tirés au sort du
   premier passage.

Les douze langues sont donc servies, et mesurées **identiques** — suite ordonnée
`(terme, offsets, position)`, vocabulaire complet plus des phrases écrites pour
leurs pièges (`tests/compat/sonde_langues.py`, 43/43 batteries).

**Le finnois reste refusé, et il est chiffré.** Le stemmer finnois de
`rust-stemmers` s'écarte de celui de Lucene sur **13 mots des 84 399** (0,015 %),
tous des emprunts à diacritique étranger : il coupe la voyelle finale que
l'algorithme garde (`garcía` → `garcí` au lieu de `garcía`, de même
`bundesstraße`, `españa`, `musée`). Un analyzer n'est jamais livré sous le nom
d'ES tant qu'il n'est pas mesuré identique : 0,015 % de documents rendus
autrement, en 200 et sans un mot, est le pire résultat possible ici. Les autres
langues d'ES (`arabic`, `czech`, `greek`, `thai`…) demandent des filtres de
normalisation qui ne sont pas portés, et sont refusées en le nommant.

**Ce que le chantier a corrigé au passage, et qui n'a rien à voir avec les
langues.** Le `lowercase` de ferrite laissait passer **32 caractères** qu'ES
replie : les 31 caractères *titre* d'Unicode (`ǅ`, `ᾈ`…), que le `LowerCaser` de
tantivy ignore parce que `is_uppercase` est faux pour eux, et le `İ` turc, dont
le repli de Rust fait **deux** caractères là où Java n'en rend qu'un. Sur
`standard` comme sur tout analyzer sur mesure, en silence et depuis toujours.
Mesuré caractère par caractère contre ES sur les 1 433 caractères qui ont une
minuscule.

**Les analyzers sur mesure**, eux, sont supportés — un mapping venu d'une
instance réelle en déclare presque toujours un, et le plus souvent avec des
briques que ferrite a :

```json
"analysis": {
  "analyzer": {"fr_produit": {"type": "custom", "tokenizer": "standard",
                              "filter": ["lowercase", "asciifolding"]}},
  "filter":   {"mes_vides":  {"type": "stop", "stopwords": ["le", "la"]}}
}
```

| | État | Détail |
|---|---|---|
| `analysis.analyzer` de type `custom` | ✅ | `tokenizer` + liste de `filter` |
| Tokenizers | 🟡 | cités par leur nom, ou **déclarés** dans `analysis.tokenizer` avec leurs paramètres. Supporté : `standard`, `whitespace`, `keyword`, `letter`, `lowercase`, `ngram`, `edge_ngram` (voir la ligne dédiée). Refusé : `pattern`, `char_group`, `path_hierarchy`, `uax_url_email`… (aucun mapping venu d'une instance réelle n'en a encore demandé) |
| `ngram` et `edge_ngram` — tokenizer **et** filtre | ✅ | la brique de l'autocomplétion « au fil de la frappe » : elle travaille à l'**indexation**, là où `match_phrase_prefix` travaille à la requête. Mesurée identique à ES sur 217 textes, **positions et offsets compris** (`diff_analyzers.py`) : un n-gramme mal positionné casse `match_phrase` sans changer le compte de tokens. Supporté : `min_gram` (défaut 1), `max_gram` (défaut 2 — sauf le filtre `edge_ngram` **cité par son nom**, qui vaut 1, le défaut de Lucene), `token_chars` (tokenizer — `letter`, `digit`, `whitespace`, `punctuation`, `symbol`, `custom`. Les classes sont celles de `Character.getType`, mesurées caractère par caractère contre ES sur 710 caractères), `custom_token_chars` (tokenizer, exigé par `token_chars: [custom]`), `side` (filtre `edge_ngram` : `front` (défaut) ou `back`. Sur le **tokenizer**, ES ne le lit pas et rend les grammes de tête ; ferrite fait pareil), `preserve_original` (filtre : ajoute le token entier, et le garde quand il est plus court que `min_gram`). Refusé : les noms de catégories Unicode fines dans `token_chars` (ES accepte aussi `math_symbol`, `connector_punctuation`… ; ferrite s'en tient aux six classes documentées et refuse le reste explicitement) |
| Filtres | 🟡 | Supporté : `lowercase` (celles de Java, pas celles de Rust : les 31 caractères **titre** (`ǅ`, `ᾈ`…) et le `İ` turc sont repliés comme chez ES, mesuré caractère par caractère. `language` est refusé — seul l'analyzer `turkish` pose des minuscules de langue), `asciifolding`, `stop` (liste explicite, `_none_`, ou le nom d'une des 14 listes servies (`_english_`, `_french_`, `_german_`…). Les listes sont **lues dans le jar de Lucene** du conteneur de référence puis vérifiées mot à mot contre ES, dans les deux sens), `stemmer` (`language` parmi les 19 noms mesurés identiques à ES (`danish`, `dutch`, `hungarian`, `norwegian`, `romanian`, `russian`, `swedish`, `turkish`, `porter2`, `german`, `spanish`, `italian`, `portuguese`, `english`, `light_french`, `light_german`, `light_spanish`, `light_italian`, `light_portuguese`). ES n'a **aucun** alias à deux lettres : `nl`, `de`, `fr` sont refusés chez lui aussi), `porter_stem` (le Porter original, cité par son nom de filtre), `elision` (`articles` (exigé) et `articles_case` — qui veut l'inverse de ce que son nom dit : c'est un `ignoreCase`, donc le défaut (`false`) compare **exactement**, et `L'anno` reste entier), `apostrophe`, `german_normalization`, `ngram`, `edge_ngram` (voir la ligne dédiée). Refusé : les stemmers qu'aucune mesure ne couvre (`finnish` (13 écarts sur 84 399 mots), `french` Snowball (79 sur 21 653 — c'est `light_french` que l'analyzer `french` pose), `dutch_kp`, `kstem`, `light_english`, `minimal_english`, `lovins`, `german2`, `minimal_german`), `stemmer_override`, `keyword_marker` (les quatre mots que `dutch` impose sont posés par son analyzer ; le filtre générique n'est pas écrit), `synonym`, `shingle`, `word_delimiter`, `truncate`… (pas encore écrits) |
| `char_filter` | ❌ | **pas encore** — aucun mapping venu d'une instance réelle n'en a encore demandé ; c'est une brique à écrire, pas un obstacle |
| Un analyzer de type autre que `custom` (`french`, `standard` paramétré…) | ❌ | **pas encore** — paramétrer un analyzer intégré (`stopwords`, `stem_exclusion`) demande de reproduire sa composition interne exacte, qui n'est mesurée que dans sa forme par défaut |

Le nom déclaré est celui que rend `_mapping`, et un analyzer sur mesure n'existe
que dans son index — `_analyze` sans index ne connaît que les intégrés, sauf à
lui donner son `tokenizer` et ses `filter` **en ligne**, comme le fait ES.

**Les n-grammes** (`ngram`, `edge_ngram`) sont la brique de l'autocomplétion
« au fil de la frappe ». Ils travaillent à l'**indexation**, là où
`match_phrase_prefix` travaille à la requête — un CMS qui propose des pages
pendant qu'on tape n'a pas d'autre moyen :

```json
"settings": {
  "index": {"max_ngram_diff": 12},
  "analysis": {
    "filter":   {"edgengram": {"type": "edge_ngram", "min_gram": 1, "max_gram": 15}},
    "analyzer": {"edgengram_analyzer": {"type": "custom", "tokenizer": "standard",
                                        "filter": ["asciifolding", "lowercase", "edgengram"]}}
  }
}
```

Deux choses qu'aucune documentation ne dit, et qui décident du résultat. La
première : le **tokenizer** avance d'une position par gramme, le **filtre** pose
tous les grammes d'un mot **à la position de ce mot**. La seconde en découle —
une phrase et un `operator: and` portent sur des **positions**, pas sur des
termes, donc les grammes d'un même mot y sont des **alternatives** et non une
suite. Les enchaîner rendait beaucoup moins de documents, en 200 ; c'est le
fuzzer différentiel qui l'a trouvé, et `diff_analyzers.py` qui le tient — 210
textes comparés sur `(terme, offsets, position)`.

Ce qui reste refusé de ce côté-là est une conséquence de tantivy, écrite plutôt
que silencieuse : une phrase de **plusieurs mots** sur un tel champ demanderait
la `MultiPhraseQuery` de Lucene, qui n'a pas d'équivalent. Un mot seul passe.

**À savoir sur l'élision.** `standard` garde `l'édition` en **un seul terme**,
des deux côtés : c'est le filtre `elision` qui le couperait, et `standard` ne le
pose pas. Chercher `edition` ne trouve donc pas `l'édition` — chez ES non plus,
avec le même analyzer. Les analyzers `french` et `italian` le posent, eux, et le
filtre se déclare aussi seul (`articles`, `articles_case`). Un piège s'y cache,
mesuré : `articles_case` veut **l'inverse** de ce que son nom dit — il est passé
à un `CharArraySet` en guise de `ignoreCase`, donc le défaut (`false`) compare
**exactement**, et `L'anno` reste entier là où `l'anno` s'élide.

**Ce que la comparaison a corrigé au passage.** `standard` — l'analyzer **par
défaut** — découpait `l'ascension` en `l` et `ascension`, là où ES garde
`l'ascension` en un seul terme : tout texte français était donc indexé
différemment. ferrite applique désormais les frontières de mots d'Unicode
(UAX#29), celles de Lucene. `stop`, lui, se construit chez ES sur le tokenizer
« lettres » et non sur `standard` (les chiffres sont donc des séparateurs).

### Types de champ

| Type ES | État | Traduction tantivy |
|---|---|---|
| `text` | ✅ | champ indexé tokenisé (positions + fréquences), tokenizer `default` |
| `keyword` | ✅ | champ indexé non tokenisé (`raw`) + fast field (tri) |
| `byte`, `short`, `integer`, `long` | ✅ | `i64` indexé + fast. Les bornes du type sont vérifiées à l'indexation |
| `float`, `double` | 🟡 | `f64` indexé + fast. Refusé : la précision d'un `float` **à l'affichage** (ES stocke un `float` sur 32 bits et l'imprime avec le plus court texte qui y revient (`2894.4688`) ; ferrite le traduit en `f64` et l'imprime entier (`2894.46875`). C'est le **même** flottant 32 bits — vérifié par [`fuzz_vs_es.py`](../tests/compat/fuzz_vs_es.py), qui compare les deux après arrondi — mais le texte JSON diffère dans un `sort` et dans une agrégation) |
| `boolean` | ✅ | `bool` indexé + fast |
| `date` | 🟡 | `date` (millisecondes) indexé + fast. **`format` supporté** : motifs Java (`yyyy`, `yy`, `MM`, `dd`, `HH`, `hh`, `mm`, `ss`, `SSS`, `a`, `Z`, texte entre apostrophes), alternatives `\|\|`, et les noms prédéfinis courants (`strict_date_optional_time`, `epoch_millis`, `epoch_second`, `date`, `date_time`, `basic_date`…). Le format sert à lire (indexation, bornes d'un `range`, ancre d'une expression de date math) **et** à rendre (`*_as_string`). Refusé : les lettres de motif non traduites (`G`, `w`, `e`… refusées explicitement plutôt qu'ignorées) |
| Tableaux de valeurs | ✅ | tout champ accepte une valeur ou un tableau |
| `null` | ✅ | ignoré à l'indexation, comme chez ES (pas de `null_value`) |
| `object` (sous-objet), déclaré ou deviné | ✅ | indexé par **chemins pointés** (`client.ville`), comme ES. Un objet n'est pas un champ : il n'existe que par ses feuilles. `GET /_mapping` re-niche les chemins. Un tableau d'objets est aplati — comme ES, la correspondance entre sous-champs d'un même élément est perdue (c'est ce que `nested` corrige) |
| `nested` | 🟡 | voir [la section dédiée](#nested) |
| `join` (parent/enfant) | 🟡 | voir [la section dédiée](#join-parentenfant) |
| Tout autre type (`geo_point`, `ip`, `binary`…) | ❌ | **pas encore** — chacun demande son propre encodage et ses propres requêtes ; aucun n'est apparu dans les mappings des instances reprises jusqu'ici |
| `analyzer` | 🟡 | sur un champ `text` — voir la section dédiée. Supporté : `standard`, `simple`, `whitespace`, `keyword`, `stop`, `english`, `french`. Refusé : les analyzers des autres langues |
| `search_analyzer` | ✅ | l'analyzer du **côté requête**, quand il diffère de celui de l'indexation. C'est le compagnon obligé des n-grammes : on indexe en grammes, on cherche le mot entier — sans lui, `elan` rend tout ce qui commence par `e`. Sur un `text` seulement (ES le refuse ailleurs comme un paramètre inconnu, et ferrite reprend sa phrase). `_analyze` avec `field`, lui, rejoue l'analyzer d'**indexation** : c'est ce que fait ES, mesuré |
| `copy_to` | ✅ | recopie la valeur **brute** du champ dans une ou plusieurs cibles à l'indexation — c'est ainsi qu'on se refait un `_all`. La cible la lit avec **son** type et son analyzer, la copie n'entre pas dans le `_source`, et elle ne se **chaîne** pas (la cible d'une cible ne reçoit rien). Une cible absente du mapping est créée dynamiquement, au type de la valeur copiée. `fields` sur une cible rend les valeurs copiées, la valeur propre d'abord puis les sources par ordre de nom. Les trois refus d'ES sont repris avec ses phrases : copier depuis ou vers un multi-field, vers un objet, ou vers un `nested` qui n'est pas celui de la source |
| `store` | 🟡 | `store: true` conserve la valeur à part du `_source`, et c'est elle que `stored_fields` rend — dans l'ordre du document, doublons compris, une date au format du champ. `store: false` est le défaut d'ES : il est accepté et **non rendu** dans le mapping, comme chez lui. Sous un `nested`, `stored_fields` ne rend rien : chez ES la valeur stockée vit dans le document enfant, invisible depuis la racine (mesuré). Changer `store` sur un champ déjà déclaré n'est pas possible, chez ES non plus. Supporté : `true` et `false`, en booléen comme en chaîne, sur un champ ou sur un multi-field. Refusé : `store sur un objet` (ES le refuse aussi (« unsupported parameters »)) |
| Multi-fields (`fields`) | ✅ | un seul niveau, comme ES. `titre.keyword` s'interroge et se trie comme un champ à part entière |
| `ignore_above` | ✅ | sur un `keyword` : au-delà, la valeur reste dans `_source` sans être indexée |
| `index` | 🟡 | `index: true` est **accepté** : c'est le défaut d'Elasticsearch, il ne demande rien de plus que ce que ferrite fait déjà, et ES lui-même ne le garde pas — un `GET /{index}/_mapping` sur un champ posé avec `index: true` rend `{"type": "keyword"}` tout court (mesuré contre 8.15.0). `index: false` reste refusé : ferrite indexerait quand même. Les deux écritures d'ES sont admises, le booléen et la chaîne. Supporté : `true`. Refusé : `false` (le champ serait indexé quand même) |
| Autres paramètres de champ (`null_value`, `doc_values`, `norms`…) | ❌ | **pas encore** — les paramètres acceptés sont `type`, `analyzer`, `search_analyzer`, `fields`, `ignore_above`, `format`, `index`, `copy_to` et `store` ; les autres sont refusés plutôt qu'acceptés sans effet, faute de quoi un `doc_values: false` laisserait croire qu'un champ n'est pas triable |
| Noms de champ pointés (`a.b`) ou réservés | ❌ | **divergence de moteur** — un point est le séparateur de chemin d'un objet : le porter dans un nom rendrait ambigu ce qu'un `client.ville` désigne. Le préfixe `_`, lui, n'est **pas** réservé — seuls le sont les champs de métadonnées d'ES (`_id`, `_source`, `_seq_no`…) et les racines des colonnes internes de ferrite (`_elem`, `_nelem`, `_join_parent`), qu'un champ utilisateur écraserait |

### `store`, `copy_to` et `search_analyzer`

Trois paramètres de mapping, et c'est un vrai client qui a dit lesquels : ce
sont exactement les trois qui restaient entre [Wagtail v7.1](application.md) et
ferrite après la livraison des n-grammes. Aucun n'est une demande vide — les
accepter en silence aurait rendu des résultats faux — et aucun n'a été deviné :
tout ce qui suit vient d'une mesure contre un vrai Elasticsearch 8.15.

**`search_analyzer`** est le compagnon obligé des n-grammes. Un champ
d'autocomplétion s'indexe en grammes (`é`, `él`, `éla`, `élan`) ; si la requête
subit le même découpage, chercher `elan` revient à chercher « `e` ou `el` ou
`ela` ou `elan` », donc tout ce qui commence par `e`. C'est le comportement d'ES
aussi, mesuré — pas un défaut, mais exactement ce que Wagtail corrige en posant
`search_analyzer: "standard"`. Deux bords viennent de la mesure : sur autre
chose qu'un `text`, ES ne connaît pas le paramètre du tout (sa phrase est
`unknown parameter [search_analyzer] on mapper [k] of type [keyword]`, et
ferrite la reprend) ; et `_analyze` avec `field` rejoue l'analyzer
d'**indexation**, jamais celui de recherche. Enfin, un champ qui déclare un
`search_analyzer` sans analyzer d'indexation se voit rendre `analyzer:
"default"` par ES — `default` étant le **nom** de l'analyzer de l'index, pas un
synonyme de `standard`, ferrite le relit comme tel : sans ça, un redémarrage
transformerait le mapping en quelque chose que personne n'a demandé.

**`copy_to`** recopie la valeur **brute** d'un champ dans une ou plusieurs
cibles, à l'indexation. C'est ainsi qu'on se refait un `_all` — le `_all_text`
de Wagtail — et la cible relit la valeur avec **son** type et son analyzer :
un `integer` copié dans un `text` s'y indexe comme `"42"`. Quatre règles, toutes
mesurées :

- la copie **n'entre pas dans le `_source`** ; elle est indexée, pas stockée ;
- elle ne se **chaîne pas** : `a → b → c` ne met rien de `a` dans `c` ;
- une cible absente du mapping se crée **dynamiquement, au type de la valeur
  copiée** — un `long` copié donne un `long`, pas un `text`. C'est la moitié du
  sujet qu'un demi-support oublierait : la copie partirait dans le vide, et la
  recherche sur `_all_text` ne rendrait rien, en silence ;
- `fields` sur une cible rend quand même les valeurs copiées, alors qu'elles ne
  sont nulle part dans le `_source` : la valeur propre de la cible d'abord, puis
  les sources **par ordre de nom**.

Les refus sont ceux d'ES, avec ses phrases : copier **depuis** ou **vers** un
multi-field, copier vers un objet, copier vers un `nested` qui n'est pas celui
de la source. La copie d'un sous-champ de `nested` vers la racine, elle, est
autorisée — et c'est exactement ce que Wagtail demande sur ses `RelatedFields`.

**`store`** conserve la valeur à part du `_source`, et c'est elle que
`stored_fields` rend. `store: false` est le défaut d'ES : comme `index: true`,
il ne demande rien, et ES ne le conserve même pas dans le mapping qu'il rend —
il est donc accepté et non rendu. Sous un `nested`, ferrite ne stocke rien :
chez ES la valeur stockée vit dans le document enfant, que `stored_fields` ne
lit pas depuis la racine, et la stocker aurait fait rendre à ferrite **plus**
qu'ES, en silence.

Changer `store` sur un champ déjà déclaré est refusé, exactement comme chez ES
(`Cannot update parameter [store] from [true] to [false]`), et pour la même
raison qu'`analyzer` : la valeur des documents déjà écrits ne changerait pas.
ferrite y ajoute `search_analyzer` et `copy_to`, qu'ES sait mettre à jour et lui
non : les accepter sans rien changer serait pire que les refuser. Redéclarer un
champ **à l'identique** reste licite — c'est ce que fait une application qui
déclare le même champ pour deux de ses modèles.

## Ingestion

| Route | État | Détail |
|---|---|---|
| `PUT\|POST /{index}/_doc/{id}` | 🟡 | `_version`, `result`, `_seq_no`, `_primary_term`, `_shards`. `op_type=create` honoré. Refusé : `require_alias` (n'écrire que si la cible est un alias), `forced_refresh` (le champ de la réponse ; ES le rend sous `refresh=wait_for`) |
| `POST /{index}/_doc` | ✅ | identifiant généré par le serveur |
| `PUT\|POST /{index}/_create/{id}` | ✅ | 409 `version_conflict_engine_exception` si présent |
| `GET /{index}/_doc/{id}` | ✅ | temps réel : une écriture non rafraîchie est visible. `_source_includes` / `_source_excludes` / `_source` supportés, et `?stored_fields=` rend les champs déclarés [`store: true`](#types-de-champ) — il retire alors le `_source`, comme sur une recherche |
| `HEAD /{index}/_doc/{id}` | ✅ | |
| `DELETE /{index}/_doc/{id}` | ✅ | 404 + `result: not_found` si absent, `_version` reste monotone |
| `POST\|PUT /_bulk`, `/{index}/_bulk` | 🟡 | NDJSON, actions `index` / `create` / `delete` / `update`, statut et erreur **par item**. Supporté : `_index` (métadonnée d'action), `_id` (métadonnée d'action ; toute **valeur simple** est convertie en texte comme chez ES (`42` → `"42"`), un objet ou un tableau est refusé). Refusé : `version`, `version_type`, `require_alias`, `_routing`, `routing`, `if_seq_no`, `if_primary_term`, `pipeline`, `retry_on_conflict`, `dynamic_templates` |
| `refresh` (`true` / `false` / `wait_for`) | ✅ | `wait_for` est traité comme `true` : le commit est synchrone et mono-shard |
| `POST /{index}/_update/{id}` | 🟡 | Supporté : `doc` (fusion partielle), `upsert`, `doc_as_upsert`, `detect_noop`. Refusé : `script` (voir le scripting, hors périmètre), `_source` (filtrer la réponse d'un `_update`), `require_alias` |
| `GET\|POST /_mget`, `/{index}/_mget` | ✅ | formes `ids` et `docs`, filtrage de `_source`, erreur par document, et `stored_fields` — en query string pour tout le lot, ou par descripteur, le sien l'emportant. Un `_id` **numérique** est lu comme la chaîne correspondante, comme chez ES |
| `GET\|POST /{index}/_count` | ✅ | avec ou sans `query` |
| Versionnage optimiste `if_seq_no` / `if_primary_term` | ✅ | 409 `version_conflict_engine_exception` si le document a bougé |
| `version` / `version_type` externes | ❌ | **pas encore** — la version d'un document est gérée par ferrite ; l'imposer de l'extérieur demande de tenir un ordre que le serveur ne contrôle plus, et rien ne l'a encore réclamé |
| `POST /{index}/_delete_by_query` | 🟡 | purge par requête, synchrone. La réponse porte les compteurs d'ES — `total`, `deleted`, `batches`, `version_conflicts`, `noops`, `retries`, `failures[]` — et son statut passe à **409** dès qu'un conflit n'est pas absorbé par `conflicts=proceed`. Le relevé se fait sur l'instantané de la recherche, et chaque écriture est conditionnée par le `_seq_no` observé : c'est ce qui produit les `version_conflicts`, comme chez ES. Supporté : `query` (obligatoire — sans lui, 400 `query is missing`), `conflicts` (`abort` (défaut) / `proceed`, en paramètre ou dans le corps), `max_docs` (en paramètre ou dans le corps ; les deux à des valeurs différentes = 400), `scroll_size` (1 à 10 000 ; change `batches` et l'endroit où `abort` s'arrête), `refresh` (`true` / `false` seulement — `wait_for` y est refusé, comme chez ES), `wait_for_completion` (`true` seulement), `slices` (`1` seulement — la valeur par défaut), `requests_per_second` (`-1` seulement — la valeur par défaut), les tolérances d'expression d'index (`ignore_unavailable`, `allow_no_indices`, `expand_wildcards`). Refusé : `wait_for_completion=false` (il rend une **tâche**, et ferrite n'a pas d'API `_tasks`), `slices` (> 1 ou `auto` : des tâches parallèles, et une section `slices` en plus dans la réponse), `slice` (dans le corps — une tranche n'a de sens qu'à plusieurs), `requests_per_second` (il régule le débit et remplit `throttled_millis`, que ferrite rendrait à zéro), `terminate_after` (il arrête la recherche sans arrêter l'écriture : `total` ne dirait plus ce qu'il dit), `sort` (dans le corps — il choisit **quels** documents `max_docs` retient), `size` (l'ancien nom de `scroll_size` dans le corps), la recherche par chaîne (`q`, `df`, `analyzer`, `default_operator`, `lenient`, `analyze_wildcard`), `routing` (ferrite est mono-shard, il n'y a rien à choisir), les paramètres de la recherche interne (`scroll`, `search_timeout`, `search_type`, `request_cache`, `stats`, `version` — la commande ne rend aucun document) |
| `POST /{index}/_update_by_query` | 🟡 | **sans script**, la route réindexe depuis le `_source` — le geste utile après un `PUT /{index}/_mapping`. La réponse porte `updated` en plus des compteurs de `_delete_by_query` ; `noops` reste à 0, parce qu'ES n'en compte que sur ordre d'un script. Sans corps du tout, la requête vaut `match_all` (contrairement à `_delete_by_query`, où l'absence de `query` est une erreur). Supporté : `query` (facultatif ici — absent, il vaut `match_all`), `conflicts` (`abort` (défaut) / `proceed`, en paramètre ou dans le corps), `max_docs` (en paramètre ou dans le corps), `scroll_size` (1 à 10 000), `refresh` (`true` / `false` seulement), `wait_for_completion` (`true` seulement), `slices` (`1` seulement — la valeur par défaut), `requests_per_second` (`-1` seulement — la valeur par défaut), les tolérances d'expression d'index (`ignore_unavailable`, `allow_no_indices`, `expand_wildcards`). Refusé : `script` (c'est Painless, hors périmètre — y compris un objet `script` vide), `pipeline` (les pipelines d'ingestion sont hors périmètre), `wait_for_completion=false` (il rend une **tâche**, et ferrite n'a pas d'API `_tasks`), `slices` (> 1 ou `auto`), `slice` (dans le corps), `requests_per_second` (il régule le débit et remplit `throttled_millis`), `terminate_after`, `sort` (dans le corps), `size` (l'ancien nom de `scroll_size` dans le corps), la recherche par chaîne (`q`, `df`, `analyzer`, `default_operator`, `lenient`, `analyze_wildcard`), `routing` (ferrite est mono-shard, il n'y a rien à choisir), les paramètres de la recherche interne (`scroll`, `search_timeout`, `search_type`, `request_cache`, `stats`, `version` — la commande ne rend aucun document) |
| `_reindex`, pipelines d'ingestion | ❌ | **pas encore** — `_reindex` copie d'un index vers un autre, et son intérêt est justement ce que ferrite n'a pas : une source distante, un pipeline, un script. Rien ne s'y oppose, ce n'est pas écrit — et `scroll` + `_bulk` le font aujourd'hui côté client, ce que ni `_delete_by_query` ni `_update_by_query` ne permettaient |

Sans `refresh`, une écriture devient visible **au plus tard après 1 seconde**
(équivalent du `index.refresh_interval` d'ES). Avec `refresh`, la visibilité est
garantie au retour de l'appel, y compris si le rafraîchissement de fond est en
train de tourner — les rafraîchissements sont sérialisés entre eux.

## Recherche

`POST|GET /{index}/_search` ✅, et `{index}` est une **expression** au sens
d'Elasticsearch — voir [Expressions d'index](#expressions-dindex-listes-motifs-alias)
juste en dessous. `POST|GET /_search` sans index cherche partout, comme `_all`.

Quand l'expression ne vise **aucun** index — cluster vide, motif sans
correspondance — le corps est quand même lu : requête, agrégations et tri sont
traduits contre un schéma vide avant qu'on conclue qu'il n'y a rien à chercher.
Ça a longtemps été faux, et c'était le seul échec silencieux connu du projet :
la traduction du Query DSL se faisant index par index, zéro index voulait dire
zéro validation, et une requête que le premier index venu refuse rendait 200.
Les seuls verdicts qui restent suspendus sont ceux qu'aucun mapping ne peut
prononcer (champ non mappé, chemin `nested`, champ `join`) — ES les diffère à
l'exécution d'un shard, et il n'y a pas de shard.

### `scroll` — l'export d'un index

C'est ce que `helpers.scan` du client officiel utilise, donc ce dont dépend tout
export : sans lui, une sauvegarde d'index échoue au premier appel.

| Route / paramètre | État | Détail |
|---|---|---|
| `?scroll=1m` sur `_search` | ✅ | ouvre un contexte et rend `_scroll_id`. Durées d'ES (`30s`, `1m`, `2h`, `500ms`…) ; sans unité, ❌ avec le message d'ES |
| `POST\|GET /_search/scroll` | ✅ | `{"scroll_id": "...", "scroll": "1m"}`, ou en query string. Le `keep_alive` est repoussé à chaque page |
| `POST\|GET /_search/scroll/{scroll_id}` | ✅ | la forme héritée, identifiant dans l'URL |
| `DELETE /_search/scroll`, `/_search/scroll/{scroll_id}` | ✅ | une liste d'identifiants ou `_all` ; `{"succeeded": true, "num_freed": n}`. Fermer deux fois n'est pas une erreur |
| `from` avec `scroll` | ❌ | **comme Elasticsearch** — comme ES : `action_request_validation_exception` |
| Contexte expiré, fermé, ou jamais ouvert | ❌ | **comme Elasticsearch** — **404** `search_phase_execution_exception`, cause `search_context_missing_exception` — la forme exacte d'ES, celle que les clients reconnaissent |

Ce que le contexte garantit, et comment :

- **chaque document une fois, et une seule** : tout ce qui correspond est balayé
  et ordonné **à l'ouverture**, une fois pour toutes ; les pages suivantes sont
  des tranches de ce tableau. La Nième page ne coûte donc pas N recherches ;
- **l'index est figé** : le `Searcher` tantivy du moment est retenu — c'est un
  instantané, et tantivy garantit que ses segments survivent à sa durée de vie.
  Ce qui est écrit pendant l'export ne s'y invite pas, et rien de ce qui existait
  ne se perd. Sans ça, un commit pendant l'export renumérote les segments et les
  adresses déjà repérées ne désignent plus les mêmes documents ;
- **les agrégations ne sont rendues qu'une fois**, sur la première page, comme
  chez ES ;
- `hits.total` et `_shards` sont les mêmes sur toutes les pages.

Le prix : un contexte vivant côté serveur (un candidat par document
correspondant). D'où le `keep_alive`, la purge des contextes expirés toutes les
30 s, et la limite de **500 contextes ouverts** (`search.max_open_scroll_context`
d'ES) — au-delà, ouvrir est refusé plutôt que de laisser un client oublieux
retenir tout l'index.

## Expressions d'index (listes, motifs, alias)

Partout où une route attend un index, elle accepte la même grammaire qu'ES.
C'est [`src/selection.rs`](../src/selection.rs) qui la résout, et **elle seule** :
un motif veut donc dire la même chose pour `_search`, `_count`, `_refresh`,
`_mapping`, `_cat/indices` et `DELETE`.

| Forme | État | Ce qu'elle désigne |
|---|---|---|
| `catalogue` | ✅ | l'index, ou l'alias, de ce nom |
| `produits,marques` | ✅ | les deux — c'est ce qu'envoie `es.search(index=["produits","marques"])` |
| `audits-2026.08.*` | ✅ | les index **et les alias** dont le nom correspond |
| `_all`, `*`, URL sans index | ✅ | tous les index |
| `audits-*,-audits-2026.07.*` | ✅ | les premiers, moins les seconds |
| `ignore_unavailable` | ✅ | un nom concret absent est ignoré au lieu d'être une erreur |
| `allow_no_indices` | ✅ | défaut `true` : un motif sans correspondance rend 0 résultat, pas 404 |
| `expand_wildcards` | 🟡 | ferrite n'a ni index fermé ni index caché : `closed` seul ne désigne donc rien. Supporté : `open`, `hidden`, `all`, `closed` (accepté, ne désigne rien). Refusé : `none` |

Un nom concret absent reste une erreur (`index_not_found_exception`), un nom
réservé aussi (`invalid_index_name_exception` sur un `_` initial) : ES fait la
même distinction, et elle est utile — `GET /_route_inconnue` doit dire « nom
invalide », pas laisser croire qu'il manque un index.

Les opérations qui portent sur **un seul** document (`_doc/{id}`, `_create`,
`_update`, `_mget`, une action `_bulk`) suivent un alias mais refusent une liste
ou un motif, comme ES.

**Comment le multi-index est exécuté.** Chaque index est un index tantivy
distinct, avec son propre schéma : la requête est donc **reconstruite** pour
chacun, exécutée séparément, et les résultats sont fusionnés. C'est le schéma
`query_then_fetch` d'ES appliqué à des index mono-shard : chaque index classe ses
documents avec **ses** statistiques, on ne rassemble que les `from + size`
meilleurs de chacun, et le classement final se fait sur ces candidats — ce que
fait ES entre shards par défaut. Deux documents que tout laisse ex æquo sont
départagés par l'index d'où ils viennent, et les index arrivent triés par nom,
donc l'ordre rendu est reproductible.

Les **agrégations** ne sont pas fusionnées sur leur résultat final : ferrite
collecte les résultats *intermédiaires* de tantivy
(`DistributedAggregationCollector`), les fusionne, et ne finalise qu'une fois.
Faire autrement rendrait faux tout `avg` (la moyenne des moyennes n'est pas la
moyenne) — c'est exactement la mécanique qu'ES applique entre ses shards.

**Les mappings hétérogènes**, eux, sont la règle dès qu'on parle d'index
quotidiens : un champ ajouté la semaine dernière n'existe que dans les index
récents. Deux comportements, tous deux mesurés sur un vrai ES :

- **dans la requête** : une clause qui cite un champ que *cet* index ne mappe
  pas devient « ne correspond à rien » **pour cet index-là**, et les clauses qui
  l'entourent continuent de compter. Écarter l'index entier serait faux : dans
  un `bool` avec `should`, on perdrait les documents que les *autres* clauses y
  trouvent. L'erreur « champ inconnu » n'est rendue que si **aucun** index visé
  ne connaît le champ — là, c'est encore une faute de frappe.
- **dans le `sort`** : ES ne fait pas échouer la recherche, il rapporte l'échec
  **de ce shard** et rend les documents des autres. ferrite fait pareil :
  `_shards.failed` est incrémenté et `_shards.failures[]` porte le
  `query_shard_exception`, index par index. Si aucun index ne sait trier, c'est
  un `search_phase_execution_exception` « all shards failed », avec une
  `root_cause` par index — le format exact d'ES.

  C'est là que sert `unmapped_type` : il dit sous quel type traiter le champ
  dans les index qui l'ignorent, plutôt que de perdre leurs documents. Mais le
  type choisi doit se **fusionner** avec celui des autres, et le garde-fou d'ES
  n'a rien d'évident — deux index dont les clés de tri ne tombent pas dans la
  même famille (`LONG` pour `byte` / `short` / `integer` / `long` / `date` /
  `boolean`, `FLOAT`, `DOUBLE`, `STRING`) font échouer la recherche **entière** :
  `Can't sort on field [x]; the field has incompatible sort types: [LONG] and
  [STRING] across shards!`. `float` et `double` n'y sont pas ensemble, ce
  qu'aucune documentation ne dit. Deux détails mesurés : l'erreur nomme le
  champ tel que le **second** index le voit — donc `__anonymous_` quand c'est
  lui qui porte l'`unmapped_type` — et elle ne tombe que si les deux index ont
  **apporté un document** (un `size: 0`, ou une requête qui ne ramène rien d'un
  côté, rendent 200 malgré le conflit). Sans ce contrôle, ferrite comparait un
  entier à une chaîne en les déclarant ex æquo : un ordre faux, en 200.

`tests/compat/diff_multi_index.py` mesure tout ça contre un vrai ES 8.15 :
**87/87 appels identiques**, 0 divergence assumée, 0 écart. Le même fichier se
lance contre **deux** Elasticsearch (`--calibrer`) pour vérifier que ses verdicts
veulent dire quelque chose : 87/87.

## Alias

| Route | État | Détail |
|---|---|---|
| `POST /_aliases` | 🟡 | `index`/`indices` et `alias`/`aliases` au singulier comme au pluriel, motifs compris. Tout ou rien, comme chez ES — c'est ce qui rend une bascule atomique. Un `remove` **désigne** des alias plutôt qu'il ne les nomme (`test_alias*`, `_all`), et ses deux règles de 404 ne sont pas la même : `must_exist: true` se vérifie **par index visé** — un `remove` sur `logs-*` échoue dès qu'un seul des index ne porte pas l'alias —, alors que le 404 par défaut est **global**, il ne tombe que si toute la requête finit sans rien faire (mesuré, `tests/compat/sonde_ecriture_alias.py`). Un seul écart assumé, sur l'**ordre** : quand plusieurs noms manquent, ES les rend dans l'ordre d'itération d'un `HashSet` de Java (`[ab1, ab2]` écrit ressort `[ab2, ab1]`), qui n'est l'ordre de rien — ferrite rend l'ordre écrit, et les noms sont les mêmes. Supporté : `add`, `remove`, `remove_index`, `must_exist`. Refusé : `filter`, `routing` (`index_routing`, `search_routing`) |
| `PUT\|POST /{index}/_alias/{nom}` et ses six autres URL (`/_alias/{nom}`, `/{index}/_alias`, `/_alias`, `_aliases` pour chacune) | ✅ | `{index}` est une expression ; `{nom}` est un nom, et un seul (une virgule y rend `Invalid alias name`, comme chez ES). Le nom de l'alias, celui de l'index, ou les deux, peuvent venir du **corps**, et le corps **remplace** alors le chemin — `PUT /inconnu/_alias/a` avec `{"index": "reel"}` pose l'alias sur `reel`, en 200. Deux refus assumés dans ce corps, tous deux du côté sûr : ES n'y lit que `index` et `alias` au **singulier** (un `indices`/`aliases` y est ignoré, et sort en « [indices] can't be empty »), et d'une **liste** JSON il ne garde que le dernier élément, en 200 — recopier ça poserait l'alias ailleurs que là où le corps le demande, sans un mot |
| `DELETE /{index}/_alias/{nom}` | ✅ | `{nom}` accepte un motif |
| `GET /_alias`, `/_alias/{nom}`, `/{index}/_alias`, `/{index}/_alias/{nom}` | ✅ | `{nom}` est une **expression** (liste, jokers, exclusions, `_all`) — voir ci-dessous — y compris le 404 « à corps de chaîne » d'ES (`{"error": "alias [x] missing", "status": 404}`), qui porte quand même les alias trouvés |
| `HEAD /_alias/{nom}`, `/{index}/_alias/{nom}` | ✅ | |
| `aliases` dans `PUT /{index}` | ✅ | posé après la création ; un alias refusé annule la création plutôt que de laisser une demande à moitié faite |
| `is_write_index` | ✅ | désigne l'index qui reçoit les écritures quand l'alias en couvre plusieurs |
| `DELETE /{alias}` | ❌ | **comme Elasticsearch** — effacer des index que le client n'a pas nommés n'est pas une suppression, c'est un accident ; ES 8 refuse de la même façon (« The provided expression [x] matches an alias, specify the corresponding concrete indices instead ») |
| `filter`, `routing`, `index_routing`, `search_routing` sur un alias | ❌ | **pas encore** — un alias filtré dont le filtre n'est pas appliqué rendrait précisément les documents qu'il est censé cacher ; le routage, lui, n'a rien à choisir sur un mono-shard |

Écrire à travers un alias qui couvre plusieurs index est refusé tant qu'aucun
`is_write_index` ne tranche — choisir à la place du client écrirait
silencieusement au mauvais endroit. La réponse porte alors le nom **concret** de
l'index, pas celui de l'alias, comme chez ES.

Un index et un alias ne peuvent pas porter le même nom (la résolution ne saurait
plus lequel désigner) ; supprimer un index le retire de ses alias ; et
`DELETE /{alias}` est refusé — effacer des index que le client n'a pas nommés
n'est pas une suppression, c'est un accident.

### L'expression de noms d'alias sur `GET /_alias/{nom}`

`{nom}` s'écrit comme une expression d'index : `a,b*,-c`, plus `_all`. Elle se
lit de gauche à droite — un terme ajoute ce qu'il désigne, un terme préfixé de
`-` retire de ce qui a déjà été retenu — et le tiret n'est une exclusion qu'à
partir du **deuxième** terme ; en première position il fait partie du nom.

Le 404 obéit à une seconde règle, qui a l'air de contredire la première :

```
GET /_alias/test_alias_1,-test                       404  alias [-test] missing
GET /_alias/test_blias_2,test_alias*,-test_alias_1   200
```

la même exclusion d'un alias qui existe, une fois refusée et une fois acceptée.
Ce qui les sépare est le **joker**. Tant qu'aucun terme n'est un motif, ES
compare la liste **écrite** à ce qu'il rend : une exclusion y figure telle
quelle, tiret compris, donc elle manque. Dès qu'un motif apparaît, la liste
écrite cède la place à une liste **résolue**, où ne restent que les noms ayant
survécu aux exclusions. Le corps du 404 porte quand même les alias trouvés : il
dit « il en manque », pas « il n'y a rien ».

Rien de tout cela n'était devinable. `tests/compat/sonde_alias.py` pose 21
expressions choisies pour séparer les lectures possibles de la règle :
**21/21 identiques** à ES 8.15.0 et à ES 7.10.2, statut, corps et message
compris.

### Ce que la suite de conformance d'Elastic trouve encore sur les alias

Mesuré, pas supposé — voir [`conformance.md`](conformance.md) :

| Ce que c'est | État | Détail |
|---|---|---|
| `GET /_cat/aliases` | ❌ | **pas encore** — route absente (`no handler found`) ; ses tests exigent en plus `h=`, `s=` et `help` sur les `_cat`, que ferrite n'a pas — 10 cas de la suite d'Elastic tombent dessus |
| `remove_index` **et** `add` d'un alias du même nom dans le même `POST /_aliases` | ❌ | **pas encore** — refusé en 400 (« an index or data stream exists with the same name as the alias ») : ferrite applique les alias avant les suppressions, là où ES calcule tout l'état puis l'applique — c'est pourtant l'usage même de `remove_index` |
| `GET /{index}/_alias` sur un index **fermé** | ❌ | **pas encore** — `_close` / `_open` sont hors périmètre, donc un index fermé n'existe pas ici |

## Réglages de cluster

| | État | Détail |
|---|---|---|
| `GET\|PUT /_cluster/settings` | 🟡 | `persistent` et `transient` (le second l'emporte), écriture plate ou imbriquée. Supporté : `action.destructive_requires_name`. Refusé : tout autre réglage (refusé avec le message d'ES (`not recognized`)), `flat_settings` (il aplatit les clés de la réponse, ferrite la rendrait imbriquée), `include_defaults` (il ajoute la section `defaults` du cluster) |
| `action.destructive_requires_name` | ✅ | `true` par défaut, **comme ES depuis la 8.0** |

Conséquence : `DELETE /audits-2026.07.*` et `DELETE /_all` sont **refusés par
défaut**, avec le message d'ES (`Wildcard expressions or all indices are not
allowed`). C'est délibéré : un projet qui purge par motif a forcément basculé ce
réglage sur son Elasticsearch, et si ferrite obéissait là où ES refuse, la
première différence de comportement entre les deux serveurs serait une
suppression de données.

### Clauses du Query DSL

| Clause | État | Détail |
|---|---|---|
| `match_all` | ✅ | `boost` |
| `match_none` | ✅ | |
| `match` | 🟡 | sur un champ non analysé, se comporte comme `term`. Voir [la recherche libre](#la-recherche-libre-multi_match) pour `lenient`. Supporté : `query`, `operator` (`or` / `and`), `boost`, `lenient`. Refusé : `fuzziness`, `minimum_should_match`, `analyzer`, `zero_terms_query`, `prefix_length`, `auto_generate_synonyms_phrase_query`, `fuzzy_transpositions`, `max_expansions` |
| `multi_match` | 🟡 | voir [la recherche libre](#la-recherche-libre-multi_match). Supporté : `query`, `fields` (**obligatoire**, avec la pondération `champ^3`), `type` (`best_fields` (défaut), `most_fields`, `phrase`, `phrase_prefix`), `operator`, `tie_breaker` (sur `best_fields`, `phrase` et `phrase_prefix` ; refusé sur `most_fields`, où ES l'accepte sans effet), `lenient`, `max_expansions`, `boost`. Refusé : `type: cross_fields`, `type: bool_prefix`, `slop`, les motifs de champ (`tit*`), `minimum_should_match`, `analyzer` |
| `match_phrase` | 🟡 | les termes dans l'ordre, adjacents. Sur un champ dont l'analyzer pose plusieurs termes à la **même position** (un filtre `ngram` ou `edge_ngram`), un mot **seul** passe — ce sont des alternatives, et ferrite en fait l'union comme Lucene ; **plusieurs mots** sont refusés explicitement (Lucene y construit une `MultiPhraseQuery` que tantivy n'a pas, et les enchaîner rendrait moins de documents en silence). Supporté : `query`, `boost`. Refusé : `slop` (voir les divergences), `analyzer`, plusieurs mots sur un champ à n-grammes (voir le détail ci-dessus) |
| `match_phrase_prefix` | 🟡 | les termes dans l'ordre, le dernier n'étant qu'un début de mot. Sur un champ `keyword`, refusée avec le message d'ES (« Can only use phrase prefix queries on text fields »). Supporté : `query`, `max_expansions` (défaut 50, comme ES), `boost`. Refusé : `slop`, `analyzer`, `zero_terms_query`, plusieurs mots sur un champ à n-grammes (même raison que pour `match_phrase` ; un mot seul passe, chaque n-gramme y étant développé par son préfixe) |
| `exists` | 🟡 | sur tous les types, y compris `text`. Un champ absent, `null`, ou un tableau vide compte comme absent, comme chez ES. Refusé : sur un `text` dont la valeur ne produit **aucun terme** (une chaîne vide, des espaces, de la ponctuation seule : ES tient un `_field_names` et compte le champ présent dès qu'il est dans `_source` ; ferrite lit l'index inversé, où ces valeurs n'ont rien laissé, et rend donc **moins** de documents. Le corriger demanderait de stocker les valeurs de chaque champ `text` une seconde fois, en colonne — trouvé par [`fuzz_vs_es.py`](../tests/compat/fuzz_vs_es.py)) |
| `term` | ✅ | forme courte et forme `{value, boost}`. Sur un champ `date`, la valeur désigne la **période** qu'elle couvre, pas un instant, et le date math y est accepté (comme chez ES). `case_insensitive` ❌ |
| `ids` | ✅ | `values`, `boost` |
| `prefix` | 🟡 | non analysée comme chez ES. Supporté : `value`, `case_insensitive` (repliement ASCII, comme ES), `boost`. Refusé : `rewrite` |
| `wildcard` | 🟡 | `*`, `?`, et `\` qui échappe le caractère suivant. Supporté : `value`, `case_insensitive`, `boost`. Refusé : `rewrite` |
| `regexp` | 🟡 | syntaxe **Lucene**, ancrée des deux côtés (voir les divergences). Supporté : `value`, `flags`, `case_insensitive`, `boost`. Refusé : les opérateurs `~`, `&`, `<n-m>`, `#` (refusés explicitement, jamais pris pour des littéraux), `rewrite`, `max_determinized_states` |
| `fuzzy` | 🟡 | Supporté : `value`, `fuzziness` (`AUTO` ou distance entière), `transpositions`, `boost`. Refusé : `prefix_length`, `max_expansions`, `rewrite` |
| `constant_score` | ✅ | `filter`, `boost` |
| `dis_max` | ✅ | `queries`, `tie_breaker`, `boost` — voir [`src/dismax.rs`](../src/dismax.rs) |
| `function_score` | 🟡 | le réglage de la pertinence : « le même match, mais les articles récents devant ». Les trois briques de calcul — les fonctions de décroissance, les modificateurs de `field_value_factor` et les six `boost_mode` — sont **verrouillées contre les classes d'Elasticsearch elles-mêmes**, exécutées dans le conteneur de référence ([`genere_scoring.py`](../tests/compat/genere_scoring.py), 47 184 points rejoués par `cargo test`). Voir [la section dédiée](#function_score-et-boosting). Supporté : `query` (facultative ; sans elle, `match_all`), `functions` (avec `filter` et `weight` par entrée), `weight`, `field_value_factor` (`field`, `factor`, `modifier` (les dix), `missing` — sur un champ numérique, `date` ou `boolean`), `gauss` (sur un champ numérique, `date` ou `boolean` ; `origin`, `scale`, `offset`, `decay`. Sur une `date`, `origin` accepte le date math et vaut `now` par défaut, `scale` et `offset` sont des durées), `exp` (mêmes paramètres que `gauss`), `linear` (mêmes paramètres que `gauss`), `score_mode` (`multiply`, `sum`, `avg`, `first`, `max`, `min` — **ignoré** quand il n'y a qu'une fonction sans `filter`, comme chez ES (mesuré)), `boost_mode` (`multiply`, `replace`, `sum`, `avg`, `max`, `min`), `max_boost`, `min_score` (comparé au score **après** le `boost` de la clause, comme chez ES (mesuré)), `boost`. Refusé : `random_score` (un score tiré au sort ne se reproduit pas d'un moteur à l'autre : il faudrait reproduire le hachage de Lucene sur des identifiants internes qui ne sont pas les mêmes), `script_score` (suppose Painless, un langage à part entière), `boost_factor` (**ES 8.15 le refuse aussi** (`field [boost_factor] is not supported`), et ferrite rend la même phrase : il a disparu en 5.0), `multi_value_mode` (seul le défaut d'ES est servi — `min`, appliqué à la **distance** et non à la valeur), plusieurs champs dans une même décroissance (ES en applique **un seul** sans dire lequel (mesuré) ; le reproduire demanderait de deviner), une décroissance sur un champ non mappé (ES la refuse aussi (`unknown field [x]`) — son `field_value_factor`, lui, l'accepte et sert son `missing`, et ferrite fait pareil) |
| `boosting` | ✅ | la démotion sans exclusion : `positive`, `negative`, `negative_boost` (obligatoire et positif, comme chez ES), `boost`. L'ensemble rendu est **exactement** celui de `positive` |
| `terms` | 🟡 | liste de valeurs, score constant comme chez ES. Sur un champ `date`, chaque valeur est une période, comme dans `term`. Refusé : les *terms lookup* (lire la liste des valeurs dans un autre document) |
| `range` | 🟡 | sur `keyword` / numérique / `date` / `boolean`. Sur un champ `date`, les bornes acceptent le **date math** (`now`, `now-1d/d`, `2026-03-15\|\|+1M`) et sont **arrondies selon leur côté** — voir [la section dédiée](#date-math-et-arrondi-des-bornes). Supporté : `gte`, `gt`, `lte`, `lt`, `boost`, `format` (lecture des bornes), `time_zone` (la borne est résolue **dans le fuseau**, arrondi compris — voir [la section dédiée](#date-math-et-arrondi-des-bornes). Sur un champ qui n'est pas une date, il est accepté sans effet, comme chez ES (mesuré)). Refusé : `relation`, un `range` sur un champ `text` |
| `bool` | 🟡 | `filter` ne contribue pas au score. Un `bool` qui n'a que des `must_not` matche tous les autres documents, comme chez ES. Supporté : `must`, `should`, `filter`, `must_not`, `mustNot` (l'écriture camelCase **dépréciée**, qu'ES 8.15 sert encore — et la seule du DSL : `minimumShouldMatch`, `adjustPureNegative`, `maxExpansions`, `caseInsensitive`, `tieBreaker`, `scoreMode` sont tous refusés chez lui (mesuré, un par un). ferrite ne rend pas l'en-tête `Warning` de dépréciation qu'ES y ajoute), `boost`, `minimum_should_match` (ses **quatre notations**, voir [la section dédiée](#minimum_should_match)). Refusé : `_name`, `adjust_pure_negative` |
| `_name` (nommer une clause), `matched_queries`, `include_named_queries_score` | ❌ | **pas encore** — nommer une clause n'a d'interet qu'avec `matched_queries` dans la reponse, qui n'est pas rendu : accepter le nom en le perdant serait promettre une information qui ne reviendra pas. Refuse dans toutes les clauses, `bool` compris — et le parametre `include_named_queries_score` (ES 8.13) avec, puisqu'il ne change **que** la forme de `matched_queries` (un objet `{nom: score}` au lieu d'une liste). Le laisser tomber dans « unrecognized parameter » le deguisait en faute de frappe : c'etait le seul defaut de cette ligne, et il comptait en regression pour la suite d'OpenSearch — la seule des trois sources a pouvoir l'exercer, le parametre etant posterieur a la 7.10.2 comme a la 2.12 d'OpenSearch |
| `query_string`, `simple_query_string`, `intervals`, `terms_set`, `script`… | ❌ | **pas encore** — `parsing_exception: unknown query [...]`, avec la liste des clauses connues — la plus regrettée est `query_string`, dont la syntaxe est un langage à part entière |

### La recherche libre (`multi_match`)

C'est la clause d'une barre « chercher par référence / nom / montant » : la même
chaîne posée sur plusieurs champs, souvent de **types différents**. Deux
paramètres y sont indispensables et manquaient, tous deux signalés par le
premier client de ferrite.

| Paramètre | État | Détail |
|---|---|---|
| `lenient` | ✅ | un champ dont le **type ne sait pas lire la valeur** cherchée est écarté de la clause au lieu de faire échouer la recherche (`"alice"` sur un `long`, une date illisible, une phrase à préfixe sur un `keyword`). Accepté sur `multi_match` et `match` — et seulement là, comme chez ES (`match_phrase`, `term`, `range` le refusent des deux côtés) |
| `type: best_fields` (défaut) | ✅ | le meilleur champ l'emporte (`dis_max`), `tie_breaker` optionnel |
| `type: most_fields` | ✅ | les scores s'additionnent ; `tie_breaker` y est refusé (il n'y a pas de meilleur champ à départager) |
| `type: phrase` | ✅ | `match_phrase` répété sur chaque champ, puis `dis_max` — exactement comme `best_fields` est `match` répété. `tie_breaker` s'applique |
| `type: phrase_prefix` | ✅ | idem, le dernier mot n'étant qu'un début de mot. `max_expansions` (défaut 50, comme ES) |
| `type: cross_fields`, `bool_prefix` | ❌ | **divergence de moteur** — le premier demande des statistiques de termes fusionnées entre champs, le second un scoring de suggestion : les approcher rendrait un autre classement que celui qu'un client attend |
| `slop` | ❌ | **divergence de moteur** — refusé quel que soit le type, pour la raison qui le fait refuser dans `match_phrase` (divergence n° 2) |
| `operator` sous `phrase` / `phrase_prefix` | 🟡 | accepté et sans effet — c'est ce que fait ES (mesuré) |

Mesuré contre un ES 8.15.0, en documents **et en ordre**
([`tests/compat/diff_relevance.py`](../tests/compat/diff_relevance.py), 213
requêtes dont une quarantaine sur ces deux paramètres) :

- avec `lenient`, la clause rend exactement ce que rendrait la même recherche
  sur les seuls champs lisibles ;
- si **aucun** champ ne sait lire la valeur, la clause ne correspond à rien —
  0 document, sans erreur, et sans rien exclure sous un `must_not` ;
- `lenient` n'accepte que `true` / `false` (booléen ou chaîne), avec le message
  d'ES sur le reste ;
- un `type` inconnu est refusé avec le message d'ES, mot pour mot (`failed to
  parse [multi_match] query type [...]. unknown type.`) ; le nom du type est
  sensible à la casse chez ES aussi.

Un champ **absent du mapping** est écarté de la liste `fields`, sans que la
clause entière devienne vide : c'est ce que fait ES. C'était l'inverse jusqu'ici
— ferrite rendait **0 document en silence** dès qu'un des champs cités n'était
pas mappé, le cas exact d'une barre de recherche qui balaie un champ qu'aucun
document n'a encore rempli.

### `minimum_should_match`

Combien de clauses `should` doivent être satisfaites. Les quatre notations
d'Elasticsearch sont acceptées, sur un `bool` comme sous un `nested` :

| Notation | Exemple | Lecture |
|---|---|---|
| entier positif | `3` | exactement ce nombre |
| entier négatif | `-1` | le nombre de clauses qu'on accepte de manquer |
| pourcentage | `75%`, `-25%` | la fraction du total, **tronquée** |
| conditions | `3<90%`, `2<-25% 9<-3` | « jusqu'à N clauses, toutes ; au-delà, cette formule » |

Absent, le paramètre vaut 1 quand le `bool` n'a que des `should`, et 0 dès
qu'il a une clause obligatoire (`must` ou `filter`) — un `must_not` **ne rend
pas** le `should` facultatif.

Et ce n'est pas seulement sa valeur par défaut : un minimum **explicite** qui
retombe à zéro (`"50%"` d'une seule clause, `0`, `-100%`) ne le rend pas
facultatif non plus. Lucene exige au moins une clause positive quand il n'y a
aucune clause obligatoire, quel que soit le minimum demandé. Sous un `nested`,
ferrite jetait alors le `should` entier et rendait un document dont un élément
satisfaisait seulement le `must_not` — trouvé par une plage de contrôle du
fuzzer, mesuré dans `sonde_msm.py`.

Les bords, tous mesurés contre un vrai ES 8.15
([`tests/compat/sonde_msm.py`](../tests/compat/sonde_msm.py), **53/53
identiques**), parce que ce sont exactement ceux que la documentation ne dit
pas :

- l'arrondi est une **troncature vers zéro**, pas un plancher : `-33%` de 3
  clauses exige les 3 (`-0,99` tronqué vaut 0), là où un plancher en exigerait
  2 ;
- un minimum **supérieur** au nombre de clauses n'est pas ramené à ce nombre :
  `150%` ou `5` sur 4 clauses ne rendent aucun document ;
- le séparateur de la forme combinée est l'**espace**, pas la virgule ;
- le `%` doit être le **dernier caractère** : `75%x` est une erreur ;
- une clause `should` sur un champ **non mappé** compte quand même dans le
  total : `100%` sur trois champs connus plus un inconnu ne rend rien.

Toute notation qui n'est pas comprise est refusée en 400. C'est la règle du
projet appliquée à son exemple canonique : ignorer ce paramètre rendrait **plus**
de documents que demandé, sans que rien ne le signale.

### `function_score` et `boosting`

Le réglage de la pertinence : « le même match, mais les articles récents
devant », « les produits en stock devant », « ne pas exclure les archives,
juste les repousser ».

Ces deux clauses posent un problème qu'aucune autre ne pose. Les autres rendent
un **ensemble** de documents et parfois un **ordre** ; celles-ci rendent une
**valeur** — le `_score` lui-même est ce que le client lit, affiche et compare.
Une formule recopiée depuis la documentation d'Elastic rend un nombre
plausible, et un nombre plausible ne se distingue pas d'un nombre juste par la
lecture.

D'où la méthode, qui est celle de la carte du `date_histogram` appliquée à
d'autres classes : **l'arbitre s'exécute, il ne se lit pas.** Le conteneur de
référence embarque un JDK *et* les jars d'ES, donc
`java -cp '/usr/share/elasticsearch/lib/*'` fait tourner telles quelles les
classes qui décident :

| Ce qui est mesuré | La classe d'ES qui répond |
|---|---|
| `gauss`, `exp`, `linear` | `GaussDecayFunctionBuilder$GaussScoreFunction` et ses deux sœurs (`processScale`, puis `evaluate`) |
| les dix `modifier` de `field_value_factor` | `FieldValueFactorFunction$Modifier` (`apply`) |
| les six `boost_mode`, `max_boost` compris | `CombineFunction` (`combine`) |

[`tests/compat/genere_scoring.py`](../tests/compat/genere_scoring.py) en tire
**47 184 points** (1 744 batteries faisant varier l'échelle, le `offset`, le
`decay` et l'origine), et
[`tests/scoring_vs_es.rs`](../tests/scoring_vs_es.rs) les rejoue dans
`cargo test`, **sans Docker**. Ça évite d'avoir à *choisir* une tolérance : la
question n'est pas « est-ce assez proche », c'est « est-ce le même `f64` ». Le
seul écart qui subsiste est **1 ULP** sur les `double`, là où le JDK et la libm
du système n'arrondissent pas `exp` ou `log` pareil — et il disparaît toujours
au passage en `float`, où l'égalité est exigée stricte.

De bout en bout, [`sonde_score.py`](../tests/compat/sonde_score.py) pose 194
questions aux deux serveurs et compare le `_score` de chaque hit,
`max_score`, le total et l'ordre : **180 identiques, 14 refus assumés, 0
écart** (`--calibrer` : 193/194 contre deux Elasticsearch, le seul écart étant
`random_score`, qui est tiré au sort). La plupart des questions partent d'une
requête dont le score de base est **exact des deux côtés** (une somme de
`constant_score`) : ce que l'égalité y mesure est bien l'arithmétique de la
clause, et rien d'autre. Les questions marquées `[bm25]` partent d'un vrai
`match`, dont tantivy et Lucene ne calculent pas le dernier bit pareil ; leur
tolérance n'est pas choisie, c'est l'écart **mesuré sur la requête nue** plus
trois arrondis de `float`.

Cinq règles qu'aucune documentation ne donne, toutes mesurées :

- **`min_score` compare le score *après* le `boost` de la clause.** On
  attendrait l'inverse, puisque le `boost` est un `BoostQuery` qui *enveloppe*
  la clause ; Lucene le fait descendre dans `createWeight`, et
  `FunctionScoreQuery` l'applique **dans** son scorer, que `MinScoreScorer`
  enveloppe ensuite. `min_score: 3` avec `boost: 10` ne coupe rien là où le même
  `min_score` sans `boost` coupe tout.
- **Une fonction unique sans `filter` fait ignorer `score_mode`.** ES construit
  alors son autre constructeur, celui qui pose `ScoreMode.FIRST`. Ça ne se voit
  que sur `avg`, le seul mode qui diffère de `first` à une fonction : sur un
  `weight: 2`, ES rend le score de base ×2 là où une moyenne pondérée rendrait
  ×1. La règle vaut aussi pour un `functions` à un seul élément, et un `filter`
  qui est un `match_all` **littéral** compte comme absent.
- **`avg` divise par la somme des poids**, pas par le nombre de fonctions. Deux
  fonctions de poids 3 et 5 font une moyenne sur 8.
- **Un document sans valeur a une distance nulle**, donc un score de
  décroissance de **1.0** : ES remplace la distance manquante par 0, il n'écarte
  pas le document. Le `field_value_factor`, lui, **fait échouer la recherche**
  si le document n'a pas de valeur et qu'aucun `missing` n'est posé — et cet
  échec-là, ferrite le reproduit (un `Scorer` ne peut pas échouer : l'incident
  est posé de côté et relu après la recherche).
- **Sur un champ multivalué, c'est la plus petite *distance* qui compte** pour
  une décroissance, et la plus petite *valeur* pour un `field_value_factor` —
  ce n'est pas la même chose dès que l'origine tombe au milieu des valeurs.

Et un refus qui n'en est pas un : **`boost_factor` est refusé par ES 8.15
lui-même** (`field [boost_factor] is not supported`, il a disparu en 5.0).
ferrite rend la même phrase — le servir reviendrait à accepter une requête
qu'un vrai Elasticsearch rejette.

`boosting` est plus simple, et son seul piège est qu'il n'en a pas :
l'ensemble rendu est **exactement** celui de `positive`, `negative` ne retire
rien. `negative_boost` est obligatoire et doit être positif ; au-dessus de 1 il
promeut au lieu de repousser, ce qu'ES accepte sans rien dire.

### Corps et paramètres de `_search`

| | État | Détail |
|---|---|---|
| `POST\|GET /{index}/_search`, `POST\|GET /_search` | ✅ | `{index}` est une **expression** au sens d'ES (voir [Expressions d'index](#expressions-dindex-listes-motifs-alias)) ; sans index, la recherche porte sur tout, comme `_all`. Une recherche qui ne vise **aucun** index (cluster vide, motif sans correspondance) valide quand même son corps : requête, agrégations et tri sont lus contre un schéma vide avant qu'on conclue qu'il n'y a rien à chercher. Écart connu, trouvé par la suite de conformance d'OpenSearch et **compté en régression** : `include_named_queries_score` (ajouté par ES en 8.13, donc absent de la suite figée d'Elastic) est refusé comme un paramètre inconnu |
| `query` | ✅ | |
| `from` / `size` | ✅ | corps ou query string. `from + size > 10000` ❌ (`max_result_window`) |
| `sort` | 🟡 | multi-clés, `asc` / `desc`, sur `keyword` / numérique / `date` / `boolean`, plus `_score` et `_doc`. Le tableau `sort` est rendu dans chaque hit, et c'est là que se lisent les valeurs absentes : sur un entier, une date ou un `boolean` ES ne rend pas `null` mais une **vraie valeur** (`9223372036854775807` / `-9223372036854775808`, donc ex æquo avec un document qui la porte pour de bon), sur un flottant `"Infinity"` / `"-Infinity"` en **chaîne**, et seul un `keyword` rend `null`. En multi-index, un champ non mappé par un des index donne un échec **de ce shard** — sauf si `unmapped_type` fournit l'échappatoire. Deux index dont le tri ne tombe pas dans la même **famille** (`LONG` pour `byte`/`short`/`integer`/`long`/`date`/`boolean`, `FLOAT`, `DOUBLE`, `STRING`) ne se fusionnent pas : ES rend 400, et `float` et `double` n'y sont **pas** ensemble. Mesuré par [`sonde_tri.py`](../tests/compat/sonde_tri.py). Supporté : `missing` (`_last` (le défaut), `_first`, ou une valeur de substitution. Les deux mots-clés sont **sensibles à la casse** — `_FIRST` est une valeur, pas un mot-clé. La substitution est lue **au type du champ** : une chaîne suit `Long.parseLong` / `Double.parseDouble` (donc `"+7"` passe et `"7.9"` non), un nombre JSON se tronque (`7.9` vaut 7, `1e300` sature). Une **date** se substitue par un nombre de millisecondes et un `boolean` par `0` / `1` : `"2020-03-01"` et `true` y rendent 400, comme chez ES), `mode` (`min` / `max` / `sum` / `avg` / `median`, insensible à la casse. Le défaut n'est pas un mode mais une règle : le **minimum** en ordre croissant, le **maximum** en décroissant. `sum` sur des entiers **déborde en silence** comme un `long` de Java (`[1, i64::MAX]` se classe sur `i64::MIN`), `avg` arrondit par le `Math.round` de Java (donc vers le haut à la demie), et `median` moyenne les deux valeurs du milieu quand elles sont en nombre pair — la colonne étant triée à l'indexation des deux côtés), `unmapped_type` (le type sous lequel traiter un champ que **cet** index ne mappe pas, plutôt que de faire échouer son shard. Tous ses documents y sont « sans valeur ». Ignoré quand l'index mappe le champ, et refusé en le nommant sur un type que ferrite ne mappe pas (`ip`, `binary`, ...) ou qui n'est pas une feuille (`object`, `nested`)). Refusé : `nested`, `numeric_type`, `format` (sur une clé de tri `date`), `ignore_unmapped` (il fait taire l'échec de shard d'un champ non mappé sous un `nested` ou un `_geo_distance` ; `unmapped_type` couvre le cas mappable, celui-ci ne se pose que sur les deux clés refusées ci-dessous), le tri par script (`_script`, et son `type`), le tri géographique (`_geo_distance`, et avec lui `unit`, `distance_type`, `pin.location`), le tri sur un champ `text` (y compris via `unmapped_type: text` — ES fait échouer le shard (« Fielddata is disabled »), ferrite aussi), les trois paramètres ci-dessus à côté de `_score` (ES les refuse aussi (`[_score] unknown field [mode]`) ; à côté de `_doc` il les **accepte et les ignore**, ferrite comme lui), les trois paramètres ci-dessus en query string (`?sort=` ne connaît que `champ:sens` chez ES comme ici) |
| `_source` | ✅ | `true` / `false`, chaîne, liste, `{includes, excludes}`, motifs `*`. Aussi via `_source_includes` / `_source_excludes` en query string |
| `fields` | 🟡 | la façon que la 7.10+ met en avant — et celle qu'envoie Kibana — de demander autre chose que le `_source` complet. Les valeurs sont lues dans le **`_source`** puis typées selon le mapping : l'ordre du document et ses **doublons** sont donc conservés (`["zoulou","alpha","alpha"]` ressort tel quel), et `{"tag": 42}` sur un `keyword` ressort `["42"]`. **La forme est ce qui compte** : chaque valeur est un tableau, même pour un champ mono-valué, et un champ absent n'a **pas de clé** — ce n'est pas une valeur nulle. Un multi-field (`titre.keyword`) est adressable, un sous-champ de `nested` se rend **groupé par élément** sous sa racine (`{"lignes": [{"ref": ["X1"]}, {"q": [5]}]}`, un élément sans valeur demandée étant omis), et un motif `*` ne ramène **pas** les métadonnées. Mesuré champ par champ par [`sonde_fields.py`](../tests/compat/sonde_fields.py). Supporté : `field` (un nom, un motif `*`, un multi-field, un chemin pointé), `format` (sur un champ `date` ; il remplace celui du mapping), `include_unmapped` (lit dans `_source` les chemins qu'aucun champ ne mappe — ce que Kibana envoie sur chaque recherche), `_id`, `_index` et `_version` nommés explicitement. Refusé : `_seq_no` et `_source` nommés dans `fields` (ES rend un **500** dessus (« Cannot fetch values for internal field ») ; un 500 ne se reproduit pas, ferrite les refuse explicitement), `?fields=` en query string (ES ne le connaît pas non plus — il le refuse comme un paramètre inconnu), un `format` qui n'est pas dans le vocabulaire de [`dateformat`](../src/dateformat.rs) (ES accepte un motif inconnu et rend une chaîne absurde (`format: "nawak"` rend `"0AM11AM24"`) ; ferrite le refuse, comme partout ailleurs où il lit un `format`) |
| `docvalue_fields` | 🟡 | la valeur telle qu'elle est **stockée en colonne**, et ce n'est pas la même que celle du `_source` : les colonnes sont triées, donc un `keyword` en ressort trié **et dédoublonné** (`["alpha","zoulou"]`) là où `fields` garde `["zoulou","alpha","alpha"]`, un numérique trié **avec** ses doublons (`[1,1,3]`), et un `float` avec la précision de son stockage sur 32 bits — ES rend `0.10000000149011612` là où le `_source` porte `0.1`. Accepté aussi en query string (`?docvalue_fields=`). Supporté : `field` (un nom, un motif, un multi-field, un chemin pointé), `format` (sur un champ `date`). Refusé : un champ `text` (il n'a pas de colonne ; ES fait échouer le shard (« Fielddata is disabled on [x] »), que le champ soit nommé ou attrapé par un motif — ferrite rend la même phrase), un `format` sur un champ numérique (ES l'interprète comme un `DecimalFormat` de Java (`format: "yyyy"` sur la valeur 1 rend `"yyyy1"`) ; ferrite ne l'imite pas), une métadonnée (`_id`, ...) (ES la refuse aussi (« Fielddata access on the _id field is disallowed »)) |
| `stored_fields` | ✅ | rend les champs que le mapping déclare [`store: true`](#types-de-champ) — et **rien** pour les autres, comme un Elasticsearch dont le mapping ne le porte pas. Il ne reconstitue rien depuis `_source` : ce serait rendre des valeurs qu'ES ne rend pas. L'ordre est celui du document, doublons compris, là où `docvalue_fields` trie et dédoublonne. Il change aussi la réponse elle-même : il **retire `_source`** (sauf `_source` explicite), `_none_` retire **aussi `_id`**, et `_none_` avec `fields` est un 400. Accepté aussi en query string. Supporté : une liste de noms, un motif, `_none_`, `?stored_fields=` en query string, sur `GET /{index}/_doc/{id}` et `_mget` aussi (en query string, et par descripteur dans le corps d'un `_mget` — celui du descripteur l'emporte sur celui de l'URL). Refusé : un champ stocké sous un `nested` (ES ne le rend pas non plus : la valeur vit dans le document enfant) |
| `highlight` | 🟡 | les fragments surlignés d'une barre de recherche. Ce qui se reproduit ici n'est pas « marquer les termes » mais le **découpage** de Lucene, et aucune de ses règles n'était devinable : les phrases (au sens d'UAX#29) sont fusionnées vers l'avant tant que la longueur reste sous `fragment_size`, et une phrase qui déborde à elle seule est re-coupée **au mot** autour de la correspondance — donc `fragment_size: 19` rend une phrase là où `20` en rend deux, sur le même texte. Un point suivi d'une **minuscule** ne termine pas une phrase (règle SB8), un point entre deux capitales non plus. Le fragment se centre sur le **milieu** de la correspondance : sur un mot isolé ça ne se voit pas, sur un `match_phrase` de quatre mots le bord gauche se décale de plusieurs mots. Quand il y a plus de fragments que `number_of_fragments`, ce sont les mieux notés par le `PassageScorer` de Lucene qui restent, puis remis dans **l'ordre du document**. Une phrase rend **une seule** marque, du premier terme au dernier. Un champ multivalué est traité valeur par valeur — un fragment ne franchit jamais la frontière entre deux valeurs — mais les fragments de toutes les valeurs sont mis en concurrence ensemble. Un champ sans correspondance est **absent** de la réponse, pas une chaîne vide. Mesuré fragment par fragment par [`diff_highlight.py`](../tests/compat/diff_highlight.py). Supporté : `fields` (un nom, un motif `*`, la forme héritée en liste d'objets ; seuls les champs `text` et `keyword` répondent, comme chez ES), `pre_tags` (seule la première balise est employée, comme le surligneur par défaut d'ES), `post_tags`, `tags_schema` (`default` et `styled` (`<em class="hlt1">`)), `number_of_fragments` (`0` rend la valeur entière, valeur par valeur), `fragment_size` (`0` rend une phrase entière ; une valeur négative retombe sur le défaut, comme chez ES), `no_match_size` (le début de la **première** valeur, étendu à la frontière de mot qui suit), `require_field_match` (`true`, le défaut : le champ n'est surligné que par ce que la requête y pose), la surcharge champ par champ de tous les réglages ci-dessus, `order: none` (le défaut : les fragments sortent dans l'ordre du document). Refusé : `type` (`fvh`, `plain` et même `unified` écrit explicitement : ferrite n'a qu'un surligneur, et un `type` accepté en silence laisserait croire qu'un autre découpage a été appliqué), `order` (`score` trie les fragments par leur note ; ES y emploie un tri **instable** (`introSort`), donc deux fragments de même note ne sortent pas dans un ordre reproductible), `require_field_match: false` (ES y cherche les termes de **toutes** les clauses dans **tous** les champs, par une extraction qui n'est pas celle du mode normal — il en documente lui-même le résultat comme approximatif, et ferrite n'en reproduit pas tous les cas. Un refus se voit ; un fragment silencieusement différent, non), `highlight_query`, `matched_fields`, `boundary_scanner`, `boundary_chars`, `boundary_max_scan`, `boundary_scanner_locale`, `fragmenter`, `encoder` (`html` échappe le texte du fragment), `force_source`, `phrase_limit`, `max_analyzed_offset`, un fragment d'un `nested` sous `inner_hits` (`inner_hits` est hors périmètre ; un sous-champ de `nested` se surligne bien depuis la racine) |
| `script_fields`, `runtime_mappings` | ❌ | **hors périmètre assumé** — les deux définissent des champs **calculés par un script Painless**, que ferrite n'exécute pas. La mesure le confirme plutôt que la supposition : sur les 444 requêtes du corpus qui portent `runtime_mappings`, **425 l'envoient vide** (des gabarits de tracks Rally), et sur les 19 non vides **18 portent un script**. L'objet **vide** est donc accepté — il ne définit aucun champ, donc ne demande rien, et ES rend la même réponse avec ou sans (mesuré) ; un objet non vide est refusé explicitement |
| `track_total_hits` | 🟡 | le total est **toujours exact** (`relation: "eq"`). Supporté : `true`, une valeur numérique. Refusé : `false` (il n'y a rien à économiser sur un total déjà exact) |
| Scoring | 🟡 | BM25 (tantivy), `_score` et `max_score` renseignés ; `null` quand un tri est demandé, comme chez ES. Les **valeurs** ne sont pas comparées à celles d'ES (les constantes diffèrent) ; c'est l'**ordre** qui l'est, par [`diff_relevance.py`](../tests/compat/diff_relevance.py). Un `term` sur un champ numérique vaut `1.0` comme chez ES (requête de points), un `keyword` et un `boolean` sont indexés sans *fieldnorm* comme chez Lucene — donc deux documents qui portent la même valeur marquent pareil, quel que soit le nombre de valeurs du champ. Refusé : l'`avgdl` de BM25 sur un champ `text` **facultatif** (Lucene calcule la longueur moyenne sur les documents **qui ont le champ**, tantivy sur **tous** les documents de l'index. Deux scores voisins peuvent alors s'inverser. Mesuré par [`fuzz_vs_es.py`](../tests/compat/fuzz_vs_es.py) ; l'ampleur est mesurée par `diff_relevance.py`. Et depuis `function_score`, cet écart de **valeur** peut devenir un écart d'**ensemble** : `min_score` est le seul endroit où le score cesse d'être un ordre pour devenir un seuil. Un `min_score` réglé sur les scores d'un vrai Elasticsearch ne coupe donc pas exactement au même endroit chez ferrite dès qu'un champ `text` est facultatif (mesuré : 0,998 chez ES contre 1,169 chez ferrite sur le même document, de part et d'autre d'un `min_score: 1`)), le score d'un `fuzzy` (tantivy le rend **constant** ; Lucene pondère chaque terme par sa distance d'édition. Les documents rendus sont les mêmes, leur ordre non) |
| Format de réponse | ✅ | `took`, `timed_out`, `_shards` (avec `failures[]` quand un index n'a pas su répondre), `hits.total.{value,relation}`, `hits.max_score`, `hits.hits[]` avec `_index` / `_id` / `_score` / `_source` / `sort` |
| `preference` | 🟡 | accepté, sans objet : il n'y a qu'un shard |
| `aggs` / `aggregations` | 🟡 | voir la section dédiée |
| `scroll` | ✅ | `?scroll=1m` ouvre un contexte figé et rend un `_scroll_id` — voir la section dédiée |
| `search_after`, `pit`, `collapse`, `knn`, `explain`, `seq_no_primary_term`, `post_filter`, `min_score`, `suggest`, `rescore`, `track_scores`, `q`, `terminate_after`, `version`, `indices_boost`, `profile`, `slice`, `stats`, `ext`, `retriever` | ❌ | **pas encore** — aucun n'est un obstacle de moteur ; `search_after` est celui qui manque le plus, pour paginer au-delà de 10 000 |
| `timeout` (paramètre et corps) | 🟡 | accepté et **vérifié**, sans objet : chez ES c'est une borne par shard au-delà de laquelle la collecte s'arrête et la réponse sort **partielle** avec `timed_out: true` ; ferrite cherche en un seul morceau, dans le processus, et rend toujours un résultat complet et `timed_out: false` — le sens sûr, puisqu'un `timeout` honoré rendrait *moins* de documents. Ce qui reste juste, c'est la forme : `1` (unité manquante), `1M`, `-2s`, `1.5s` sont refusés avec les phrases d'ES, `0`, `-1`, `1D` et `1MS` acceptés — tous mesurés. C'est le seul des quatre manques de cette famille qui n'a pas été trouvé par une suite de conformance mais par la **suite de tests du client go** |
| `ignore_unavailable`, `allow_no_indices`, `expand_wildcards` | ✅ | voir [Expressions d'index](#expressions-dindex-listes-motifs-alias) — `expand_wildcards=none` reste ❌ |
| `routing`, `filter_path`, `typed_keys` | ❌ | **hors périmètre assumé** — ferrite est mono-shard, `routing` n'a rien à choisir ; les deux autres changent la forme de la réponse, et une forme qui dépend d'un paramètre est une seconde API à mesurer |
| `search_type`, `max_concurrent_shard_requests`, `pre_filter_shard_size`, `batched_reduce_size` | ❌ | **hors périmètre assumé** — ils reglent la façon dont une recherche se distribue entre shards ; il n'y en a qu'un, donc rien à distribuer et rien à régler |
| `rest_total_hits_as_int` | ❌ | **pas encore** — il change la forme de `hits.total` (nombre au lieu d'objet) ; ES 8 l'accepte encore et du code venu de la 6.x/7.x s'en sert, voir [`compat-es7.md`](compat-es7.md) |
| `_msearch`, `_search/template`, `_explain` | ❌ | **pas encore** — `_msearch` est le plus regretté : un tableau de bord qui pose six facettes fait six appels au lieu d'un. Les deux autres sont des outils de mise au point |
| `GET\|POST /{index}/_field_caps` | 🟡 | par champ, son type, `searchable` et `aggregatable`, et l'agrégation **par index** quand plusieurs sont visés — c'est la question que pose un outil de découverte avant de proposer un filtre qui échouerait sur la moitié des index. Toute l'information est déjà dans le mapping : ferrite n'a ni `index: false` ni `doc_values: false`, donc les deux drapeaux se déduisent du type. Supporté : `fields` (dans l'URL ou dans le corps, jokers compris), `include_unmapped`, `index_filter` (n'décrire que les index qui ont au moins un document correspondant). Refusé : les champs de métadonnées (`_id`, `_index`, `_seq_no`…) (ES les rend sur `fields=*` ; ferrite ne sait pas les interroger, et les annoncer `searchable` serait un résultat faux), `runtime_mappings` |
| `GET\|POST /{index}/_validate/query` | 🟡 | le traducteur du Query DSL rendu observable, sans exécuter. Les deux formes de réponse d'ES sont reproduites, et la distinction compte : une requête mal formée (clause inconnue) rend `valid: false` **sans** `_shards`, une requête que ce mapping-là ne sait pas construire rend `_shards` et une explication par index. Supporté : `explain`, `all_shards` (un shard par index : sans objet ici), `ignore_unavailable`, `allow_no_indices`, `expand_wildcards`. Refusé : `rewrite` (il demande la forme **réécrite** de la requête Lucene, que ferrite n'a pas), `q` (la recherche par chaîne (`query_string`) n'est pas implémentée ; `df`, `default_operator`, `analyzer`, `analyze_wildcard` et `lenient` la suivent) |

Les paramètres purement cosmétiques `pretty`, `human` et `error_trace` sont
acceptés partout ; `pretty` est implémenté (indentation de la réponse).

**Tout paramètre de query string non reconnu est refusé** avec
`request [...] contains unrecognized parameter: [...]`, comme chez ES.

### Ce que la réponse transporte : `fields`, `docvalue_fields`, `stored_fields`

Trois façons de demander autre chose que le `_source` complet, et elles **ne
lisent pas au même endroit**. C'est ce qui les sépare, et rien de ce qui suit
n'était devinable — tout vient de
[`sonde_fields.py`](../tests/compat/sonde_fields.py), qui pose 107 questions aux
deux serveurs et compare le **hit entier** : le bloc `fields` clé par clé, la
présence de `_source`, la présence de `_id`. **100/107 identiques, 3 refus
assumés écrits, 4 différences d'ordre assumées (n° 18 ci-dessous), 0 écart.**

**`fields` lit le `_source`**, puis type chaque valeur selon le mapping. C'est
la façon que la 7.10+ met en avant, et celle qu'envoie Kibana. La **forme** est
ce qui compte pour un client : chaque valeur est un **tableau**, même pour un
champ mono-valué, et un champ absent n'a **pas de clé** — ce n'est pas une
valeur nulle. Un client qui connaît cette forme lirait mal un scalaire, sans
que rien ne le signale.

Parce qu'il lit le `_source`, `fields` garde l'**ordre du document et ses
doublons** : `["zoulou","alpha","alpha"]` ressort tel quel. Et parce qu'il type
selon le mapping, `{"tag": 42}` sur un `keyword` ressort `["42"]`. Un
multi-field (`titre.keyword`) est adressable et lit la valeur de son parent ; un
sous-champ de `nested` se rend **groupé par élément** sous sa racine
(`{"lignes": [{"ref": ["X1"]}, {"q": [5]}]}`), un élément qui ne porte aucune
des valeurs demandées étant omis. Un motif `*` ne ramène **pas** les
métadonnées ; `_id`, `_index` et `_version` nommés explicitement, si.

**`docvalue_fields` lit les colonnes**, et une colonne n'est pas un `_source` :
elle est **triée**. Un `keyword` en ressort donc trié **et dédoublonné**
(`["alpha","zoulou"]`) là où `fields` rend `["zoulou","alpha","alpha"]` ; un
numérique trié **avec** ses doublons (`[1,1,3]`) ; et un `float` avec la
précision de son stockage sur 32 bits — ES rend `0.10000000149011612` là où le
`_source` porte `0.1`. ferrite range les deux en `f64` : sans repasser par
`f32`, il rendait `0.1`, et deux serveurs qui ont indexé la même chose ne
rendaient pas la même valeur. Un champ `text` n'a pas de colonne : ES fait
échouer le shard, ferrite rend la même phrase.

**`stored_fields` lit les champs stockés**, ceux que le mapping déclare
`store: true` (voir [ci-dessous](#store-copy_to-et-search_analyzer)). Il ne
reconstitue rien depuis le `_source` : ce serait rendre des valeurs qu'ES ne
rend pas, et c'était le seul choix disponible tant que `store` était refusé.
Comme `fields`, il garde l'**ordre du document et ses doublons** — mais il les
lit ailleurs, et ça se voit sur les bords : un champ que le mapping ne stocke
pas n'a **pas de clé** là où `fields` en aurait une, une valeur écartée par
`ignore_above` n'est ni indexée ni stockée, et sous un `nested` il ne rend
**rien** (chez ES la valeur vit dans le document enfant, invisible depuis la
racine). Un `float` stocké ressort `0.1` là où sa colonne rend
`0.10000000149011612` : Lucene le range sur 32 bits et le rend par le plus court
texte qui s'y relit.

`stored_fields` change aussi la réponse elle-même : il **retire `_source`** (sauf
`_source` explicite), `_none_` retire **aussi `_id`**, et `_none_` avec `fields`
est un 400. Il n'est pas réservé à `_search` : `GET /{index}/_doc/{id}` et
`_mget` lisent les mêmes champs stockés, au même endroit — le livrer pour la
seule recherche en aurait fait un paramètre qui marche « sauf là ». Sur ces deux
routes-là, en revanche, `_none_` mélangé à d'autres noms n'est **pas** une
erreur : ES l'y ignore et rend les champs cités (mesuré). Deux bords sont venus de la suite de conformance d'Elastic plutôt
que d'ici : `_source` **cité dans la liste** est un nom de champ stocké comme un
autre, donc le citer ramène le `_source` ; et `_none_` mélangé à d'autres noms
est une erreur (`cannot combine _none_ with other fields`), pas un `_none_` qui
gagne.

`script_fields` et `runtime_mappings` restent refusés : les deux définissent des
champs calculés par un script Painless, hors périmètre. La décision n'est pas de
principe, elle est mesurée sur le corpus d'usage : sur les 444 requêtes qui
portent `runtime_mappings`, **425 l'envoient vide** — des gabarits de tracks
Rally — et sur les 19 non vides, **18 portent un script**. L'objet **vide** est
donc accepté (il ne définit aucun champ, donc ne demande rien, et ES rend la même
réponse avec ou sans) ; un objet non vide est refusé explicitement.

### Les fragments surlignés (`highlight`)

Ce qui se reproduit ici n'est pas « marquer les termes » : c'est le
**découpage** du `UnifiedHighlighter` de Lucene, tel qu'Elasticsearch le
configure. Rien de sa forme n'était devinable, et une lecture naïve —
« un fragment = une phrase », ou « un fragment = `fragment_size` caractères » —
rend systématiquement autre chose. Tout ce qui suit est mesuré contre un
ES 8.15 par
[`diff_highlight.py`](../tests/compat/diff_highlight.py) (**233 questions
posées aux deux serveurs, comparées fragment par fragment**), et étalonné
contre deux Elasticsearch avant de servir.

**Où le fragment commence et finit.** Les phrases sont fusionnées **vers
l'avant** tant que la longueur totale reste sous `fragment_size` ; si une seule
phrase déborde déjà, elle est re-coupée **au mot** autour de la correspondance.
Sur le même texte, `fragment_size: 19` rend une phrase et `20` en rend deux. Le
fragment se centre sur le **milieu** de la correspondance, pas sur son début :
sur un mot isolé les deux se confondent, sur un `match_phrase` de quatre mots le
bord gauche se décale de plusieurs mots.

**Où une phrase s'arrête.** C'est UAX#29, et deux de ses règles décident de
presque tout :

- un point suivi d'une **minuscule** ne termine pas une phrase (règle SB8).
  « zzz cible. aaa. bbb cible cible. » est **une seule** phrase — donc ES y rend
  trois fragments coupés au mot, là où « une phrase par fragment » en rendrait
  trois autres ;
- un point entre deux capitales non plus (`U.S.A.`), ni entre deux chiffres
  (`8.15`).

**Où un mot s'arrête** — et là, ce n'est **pas** UAX#29 : c'est le
`BreakIterator` du JDK, dont les jointures diffèrent sur des caractères
courants. Mesurées une par une (`no_match_size: 1` dit où tombe la première
frontière) : `abcde-fghij` et `abcde"fghij` sont **un** mot, `abcde:fghij` et
`abcde’fghij` en font deux — l'inverse de ce que dit UAX#29 pour les deux
premiers. Sans le tiret, `tiret-bas` se coupait en « tiret ».

**Ce qui est marqué.** Les termes que la requête pose sur **ce champ-là**, et
seulement ceux qui ont vraiment fait correspondre **ce document-là** : un
`should` placé dans un `bool` dont le `filter` échoue ne marque rien, et un
`bool` porteur d'un `must_not: {match_all}` ne marque jamais rien. Une phrase
rend **une seule** marque, du premier terme au dernier.

`require_field_match: false` — qui ferait chercher les termes de **toutes** les
clauses dans **tous** les champs — est **refusé**. ES lui-même documente son
résultat comme approximatif, et ferrite n'en reproduit pas tous les cas : un
`range` sur un champ non textuel y voit son automate appliqué aux termes des
autres champs (`{"range": {"drapeau": {"lt": true}}}` marque « AlphA » dans un
`keyword` voisin, parce que `"AlphA" < "T"`), et une clause qui n'a rien trouvé
dans son propre champ y marque parfois ailleurs et parfois pas. Un refus se
voit ; un fragment silencieusement différent, non.

**Quels fragments survivent** à `number_of_fragments` : les mieux notés par le
`PassageScorer` de Lucene (un BM25 dont le « document » est le fragment, pivoté
sur 87 caractères), puis remis dans **l'ordre du document**. Le `freq()` y vaut
**1** — c'est ce que rend Lucene quand le surligneur travaille sur les
`Matches` — et ça n'est pas un détail : prendre le vrai nombre d'occurrences
rend le poids négatif dès qu'un terme apparaît plus de trois fois, ce qui
**inverse** le classement.

**Un champ multivalué** est traité valeur par valeur — un fragment ne franchit
jamais la frontière entre deux valeurs — mais les fragments de toutes les
valeurs sont mis en concurrence ensemble. `no_match_size` ne lit que la
**première valeur non vide**.

**Ce que le `_source` ne dit pas.** Une valeur écartée par `ignore_above` n'a
pas été indexée : elle n'est pas surlignée, et `no_match_size` ne la rend pas
non plus. À l'inverse, la valeur qu'un `copy_to` dépose dans sa cible n'est
**nulle part** dans le `_source` de celle-ci, et elle est bien surlignée — même
règle que pour `fields`.

Un champ sans correspondance est **absent** de la réponse : ce n'est pas une
chaîne vide. Un champ qui n'est ni `text` ni `keyword` ne répond pas, même sous
un motif `*`.

### Date math et arrondi des bornes

Une borne de date d'une requête n'est pas une date : c'est une expression que le
serveur résout, et qu'il **arrondit selon le côté de la borne**. Les deux moitiés
comptent autant l'une que l'autre — la première parce que sans elle un filtre
`{"range": {"fin": {"lt": "now"}}}` échoue en 400, la seconde parce que sans elle
il rend *moins de documents* qu'ES sans que rien ne le signale.

Tout ce qui suit est mesuré contre un ES 8.15.0
([`tests/compat/diff_datemath.py`](../tests/compat/diff_datemath.py),
**276/276 bornes identiques**, messages d'erreur compris).

| Forme | État | Détail |
|---|---|---|
| `now` | ✅ | résolu **une fois par recherche**, comme ES sur son nœud coordinateur : deux bornes de la même requête parlent du même instant |
| `now±<n><unité>` | ✅ | unités `y`, `M`, `w`, `d`, `h` et `H`, `m`, `s`. `+1M` sur le 31 janvier donne le 28 février (le jour est ramené au dernier du mois), comme Java |
| `now/<unité>` | ✅ | arrondi ; `/w` arrondit au **lundi**. Sous une borne haute (`gt`, `lte`), chaque `/` rend le **dernier instant** de la période (`2026-03-15\|\|/M` sous `lte` = 31 mars 23:59:59.999) |
| `<ancre>\|\|<opérations>` | ✅ | l'ancre est lue avec le `format` du champ (ou celui de la requête), et toujours **arrondie vers le bas**, même sous un `lte` |
| date partielle (`2026-03-15`, `2026-03`, `2026-03-15T12`) | ✅ | les champs d'heure absents sont remplis au maximum sous une borne haute (`lte: "2026-03-15"` couvre la journée), les champs de **date** absents restent au minimum (`2026-03` → le 1er, pas le 31) |
| `format` sur `range` | ✅ | remplace le format du champ pour **lire les bornes** ; il ne s'applique pas à `now` |
| dans `term`, `terms`, `match`, et sous un `nested` | ✅ | une date y désigne la période qu'elle couvre : `{"term": {"d": "2026-03-15"}}` rend toute la journée, comme chez ES |
| à l'indexation | ❌ | **comme Elasticsearch** — `{"d": "now"}` est refusé, comme chez ES : le document porterait une date qui dépend de l'instant où il a été écrit |
| `time_zone` | ✅ | la borne est **résolue dans le fuseau**, arrondi compris : une date écrite sans décalage (`2026-03-29`) est minuit **local**, `now/d` arrondit au jour local, et `lte: "2026-03-29"` couvre la journée locale — qui ce jour-là, à Paris, dure 23 heures. Une date qui porte déjà son décalage (`…Z`, `+02:00`) ou un nombre d'époque désigne un instant : le fuseau ne la déplace pas (mesuré). Les règles de changement d'heure viennent du tzdb du **JDK d'Elasticsearch** ([`fuseau.rs`](../src/fuseau.rs)), et une heure locale qui n'a jamais existé est décalée de la durée du trou, comme le fait Java |

Une expression malformée est refusée avec **le message d'ES, mot pour mot**
(`unit [q] not supported for date math [-1q]`, `truncated date math [/]`,
`operator not supported for date math [1d]`, `For input string: "…"`). ES les
rend sous un `search_phase_execution_exception` « all shards failed » dont la
`root_cause` porte ce texte ; ferrite rend l'erreur directement, sans cet
empilement.

**`time_zone` déplace la borne *et* ce que l'arrondi veut dire.** Tout le calcul
se fait en heure locale, et c'est le résultat qui est reposé sur l'axe du temps :
une date écrite sans décalage (`2026-03-29`) est minuit **local**, `now/d`
arrondit au jour local, et `lte: "2026-03-29"` couvre la journée locale — qui ce
jour-là, à Paris, dure 23 heures. Une date qui porte déjà son décalage (`…Z`,
`+02:00`) ou un nombre d'époque désigne un instant, et le fuseau ne la déplace
pas : c'est ce que fait ES, mesuré. Les deux cas que le changement d'heure
fabrique suivent `ZonedDateTime.ofLocal` de Java : une heure locale qui a existé
**deux fois** prend la première, une heure locale qui n'a **jamais** existé est
décalée de la durée du trou.

## Agrégations

Comparées champ par champ à un vrai ES 8.15 sur 73 requêtes
(`tests/compat/diff_aggs.py`), clés de réponse comprises. Ce qui sépare un
`terms` d'une **facette** — `include` / `exclude` et l'ordre par
sous-agrégation — a en plus sa propre sonde,
[`sonde_facettes.py`](../tests/compat/sonde_facettes.py), qui compare le bloc
`terms` entier sur 170 questions.

| Agrégation | État | Détail |
|---|---|---|
| `min`, `max`, `sum`, `avg`, `value_count`, `stats` | ✅ | `field`, `missing`. Sur un champ `date`, la valeur est en millisecondes et le `*_as_string` est rendu comme chez ES. Les trois qui accumulent (`sum`, `avg`, `stats`) le font en `double` avec la compensation de Kahan, **comme ES**, et lisent les valeurs d'un document multivalué dans le même ordre que lui (croissant) — sans quoi le résultat diffère au-delà de 2^53 : voir la divergence n° 22 pour le seul cas qui reste. Refusé : une somme qui n'est pas **finie** (au-delà de 1,8 × 10³⁰⁸ une somme de `double` déborde ; ES rend alors la chaîne JSON `\"Infinity\"`, ferrite rend `null` — voir la divergence assumée n° 22) |
| `terms` | 🟡 | `sum_other_doc_count` est renseigné — **après** filtrage, comme chez ES : les documents des termes qu'un `include` / `exclude` écarte n'y sont pas du tout. `doc_count_error_upper_bound` suit la règle d'ES : `-1` quand l'ordre ne classe pas par compte décroissant (`_count` **croissant**, ou une sous-agrégation) **et** que le nombre de termes distincts atteint `shard_size` (`size × 1,5 + 10` par défaut), `0` partout ailleurs — `_key` compris, dans les deux sens. Sur un champ `date`, la clé du bucket est rendue en millisecondes avec son `key_as_string`, comme chez ES. Supporté : `field`, `size`, `shard_size`, `min_doc_count` (sa valeur par défaut (`1`) seulement — voir ci-dessous), `order` (`_count`, `_key`, et le chemin d'une **sous-agrégation métrique** — `{"prix_moyen": "desc"}` sur un `avg`, `{"stats_prix.avg": "desc"}` sur un `stats` (les cinq valeurs `count` / `min` / `max` / `avg` / `sum`). Une métrique à valeur unique s'écrit aussi bien `pm` que `pm.value`. Le sens se lit sans égard à la casse, comme chez ES. Les ex æquo sont départagés par **clé croissante dans les deux sens**, et un seau dont la métrique n'a aucune valeur se classe là où ES le met : comme `NaN` sous un `avg` (donc en tête d'un `desc`), comme `+∞` sous un `min`, comme `-∞` sous un `max`), `include` (sur un champ `keyword`, dans les deux formes d'ES : une **expression régulière** dans la syntaxe de Lucene, ancrée sur le terme entier (`^` et `$` y sont des littéraux) et traduite par le même code que celle d'une requête `regexp` — donc avec les mêmes quatre opérateurs refusés ; ou une **liste exacte de valeurs**, dont chaque élément est lu comme du texte (`include: [1]` cherche le terme `"1"`, comme chez ES)), `exclude` (les deux mêmes formes, et les deux peuvent être posées ensemble), `missing` (sur un `keyword` ou un numérique. La valeur est posée **au type du champ**, comme chez ES : `missing: 0` sur un `keyword` y devient la clé `"0"`, et un `"3"` sur un `long` la clé `3`). Refusé : une valeur de remplissage sur un champ `date` ou `boolean` (tantivy ne lit pas la date et rangerait ces documents sous `1970-01-01`, en 200 et sans un mot ; sur un booléen il n'arrive pas à poser la valeur du tout. Une valeur de remplissage placée au mauvais endroit se lit comme une donnée), une valeur de remplissage d'un type que le champ ne sait pas lire (un `1.5` sur un `long` promeut chez ES **toutes** les clés du bucket en flottant ; ES refuse les autres cas, ferrite aussi), `min_doc_count` autre que sa valeur par défaut (`1`) (à `0`, il demande un bucket pour les valeurs que la recherche n'a **pas** trouvées, et l'agrégation de tantivy ne le rend pas de façon fiable : zéro bucket sur une colonne numérique, zéro bucket quand la requête ne ramène rien, et des buckets vides privés de leurs sous-agrégations. Au-delà de `1`, c'est `sum_other_doc_count` qui ne suit plus : la règle d'ES a été cherchée pour de bon, une formule ajustée sur quinze formes d'un corpus les collait toutes puis s'est effondrée sur d'autres (27 écarts sur 1 450 cas tirés au sort). Elle dépend de l'ordre demandé, de la troncature et de l'ordre de parcours du dictionnaire de termes — c'est le collecteur d'ES qu'il faudrait réécrire, et annoncer un compte faux serait pire. Mesuré par [`fuzz_vs_es.py`](../tests/compat/fuzz_vs_es.py)), un filtre de termes — inclusion ou exclusion — sur un champ qui n'est **pas** textuel (ES sert la liste exacte sur un numérique, une date et un booléen (`include: [1, 3]` sur un `long` rend deux seaux) ; l'agrégation de tantivy, elle, ne filtre les termes que sur une colonne de chaînes et **écarte la colonne entière** dès qu'elle ne l'est pas — elle rendrait zéro seau, en 200 et sans un mot. L'expression régulière, elle, est refusée **des deux côtés** : le message d'ES est repris mot pour mot), un filtre de termes posé en même temps qu'une valeur de remplissage (le seau de remplissage de `missing` n'a pas d'identifiant dans le dictionnaire de termes : le filtre de tantivy l'écarte toujours, alors qu'ES le traite comme un terme ordinaire — il reste sous un `exclude` qui ne le vise pas, et il sort sous un `include` qui le nomme. Perdre en silence les documents sans valeur est exactement ce qu'une facette ne doit pas faire), la forme partitionnée (`{"partition": n, "num_partitions": m}`) (elle retient un terme selon un **hachage** de sa valeur — `Math.floorMod(murmurhash3_x86_32(terme, 31), num_partitions)`, mesuré contre ES 8.15 et stable à son redémarrage. La règle est donc connue ; ce qui manque est un moyen de l'exprimer à tantivy, dont le filtre ne connaît qu'une expression régulière ou une liste exacte. Il faudrait énumérer tout le dictionnaire de termes pour en dresser la liste, ce qui défait la raison d'être du paramètre — parcourir un champ à très forte cardinalité sans tout charger), un chemin d'ordre à plusieurs niveaux (`filtre>prix`) (ES descend à travers une agrégation mono-seau ; la seule qu'implémente ferrite (`filter`) est déjà refusée sous une agrégation de seaux), l'ordre par une agrégation mono-seau (ES classe alors les seaux sur son `doc_count`, que le chemin le nomme ou non ; la seule agrégation mono-seau que ferrite serve est déjà refusée sous une agrégation de seaux, donc le chemin ne mène nulle part), l'ordre par une agrégation de **seaux**, ou par une clé que la métrique ne rend pas (ES refuse aussi, avec le même statut — mais seulement quand il a **deux seaux à comparer** : le chemin n'est résolu qu'au moment de trier, donc à zéro ou un seau il rend 200 sur la même demande fautive. ferrite valide avant d'exécuter : voir la divergence assumée n° 23), `collect_mode`, `execution_hint`, `script`, `shard_min_doc_count`, `show_term_doc_count_error` |
| `range` | 🟡 | `ranges` avec `from` / `to` / `key`, `keyed`. Sur un champ `date`, les bornes s'écrivent **en dates** (au `format` du champ) et les buckets rendent `from_as_string` / `to_as_string`. Les intervalles que le client n'a pas demandés sont écartés : tantivy comble les trous entre deux bornes, Elasticsearch non. Refusé : un **trou** entre deux intervalles, sur un champ `date` (tantivy comble les trous et ferrite écarte ensuite le bucket de remplissage ; sur une date, où les bornes passent en nanosecondes, ce remplissage avale l'intervalle demandé. Sur un champ numérique, les deux buckets sortent et le filtrage suffit), des intervalles qui se **chevauchent** (ES compte alors un document dans chaque bucket qui le contient ; l'agrégation de tantivy partitionne les valeurs et ne sait pas le faire), un champ **multivalué** (voir la ligne suivante) |
| `histogram` | 🟡 | `interval`, `offset`, `min_doc_count`, `hard_bounds`, `extended_bounds`, `keyed`. Refusé : un champ **multivalué** (voir la ligne suivante) |
| `date_histogram` | 🟡 | les seaux sont **calculés par ferrite** ([`histodate.rs`](../src/histodate.rs)) : une pré-passe mesure le premier et le dernier instant, les bornes sont déroulées par l'arrondi d'Elasticsearch reproduit ([`calendrier.rs`](../src/calendrier.rs)), et tantivy exécute le `range` contigu qui en résulte — donc avec ses sous-agrégations et sa fusion multi-index. C'est ce qui rend `calendar_interval` et `time_zone` possibles : un mois civil n'a pas de durée constante, et un jour à Paris en a deux (23 h et 25 h). Supporté : `field`, `fixed_interval`, `calendar_interval` (`second`/`1s`, `minute`/`1m`, `hour`/`1h`, `day`/`1d`, `week`/`1w`, `month`/`1M`, `quarter`/`1q`, `year`/`1y` — **une** unité, jamais un multiple, comme chez ES), `time_zone` (identifiant IANA (`Europe/Paris`) ou décalage (`+05:30`). Les seaux suivent l'heure locale : un jour de changement d'heure dure 23 ou 25 heures, un `fixed_interval: 3h` posé sur la nuit d'octobre dure quatre heures réelles, et `key_as_string` porte le décalage local (`2026-03-01T00:00:00.000+01:00`)), `format` (rend `key_as_string` à ce format, dans le fuseau), `offset`, `min_doc_count`, `hard_bounds`, `extended_bounds`, `keyed` (`extended_bounds` et `hard_bounds` sont lues **dans le fuseau** puis arrondies ; la borne haute de `hard_bounds` est exclue, celle d'`extended_bounds` incluse (mesuré)). Refusé : `order` |
| Sous-agrégations | ✅ | sur tous les types de buckets, vérifiées jusqu'à trois niveaux. Un bucket **vide** porte les siennes, comme chez ES : tantivy comble les trous d'un `histogram` sans exécuter ce qu'il y a dessous, et ferrite y remet la forme « zéro document » — mesurée sur une recherche qui ne ramène rien, pas écrite à la main. Un bucket **rare** porte les siennes aussi, ce qui n'a pas toujours été vrai : tantivy 0.26.1 perdait ses documents au-delà de 2 048 par segment, en 200 et avec le bon `doc_count` à côté. Le correctif d'amont est épinglé (voir [`tantivy-patch.md`](tantivy-patch.md)), et 46 combinaisons parent × sous-agrégation le tiennent contre un vrai ES ([`sonde_sous_aggs.py`](../tests/compat/sonde_sous_aggs.py)) |
| `histogram`, `date_histogram`, `range` sur un champ **multivalué** | ❌ | **divergence de moteur** — l'agrégation de tantivy compte les **valeurs**, Elasticsearch compte les **documents** : un document dont le champ vaut `[1, 2, 3]` tombe trois fois dans le bucket qui les contient (mesuré : `doc_count` de 4 là où ES en compte 2). Le refus n'est prononcé que si la colonne est réellement multivaluée — un champ à une valeur par document, le cas courant, reste servi et exact. `terms`, `value_count` et `stats` ne sont pas concernés : leurs comptes coïncident avec ceux d'ES |
| `cardinality` | ❌ | **divergence de moteur** — l'estimation de tantivy diffère de celle d'ES (mesuré : 582 valeurs distinctes annoncées là où ES en compte 598), y compris sous le seuil où ES est exact — un compte approché sous le nom d'ES serait faux sans le dire |
| `filter` | 🟡 | n'importe quelle requête du Query DSL, avec ses sous-agrégations. **Exécutée par ferrite**, pas par tantivy : compter les documents qui correspondent à la recherche *et* au filtre, c'est exécuter l'intersection des deux requêtes (voir les divergences). Refusé : sous une agrégation de buckets (il faudrait rejouer sa requête bucket par bucket) |
| `percentiles`, `extended_stats`, `top_hits`, `composite`, `filters`, `nested`, `significant_terms`, `date_range`, `ip_range`… | ❌ | **pas encore** — `filters` (la sœur plurielle de `filter`) et `top_hits` sont les deux qui manquent le plus ; aucune n'est un obstacle de moteur |

Agréger sur un champ `text` est refusé, comme chez ES (`Fielddata is disabled`) :
utiliser son multi-field `.keyword`.

**`filter` est exécutée par ferrite, pas par tantivy.** Elle était refusée pour
une bonne raison — celle de tantivy prend une chaîne dans *sa* syntaxe de
requête, pas une requête du Query DSL, et la traduction serait approximative.
Mais rien n'oblige à passer par elle : compter les documents qui correspondent à
la recherche **et** au filtre, c'est exécuter l'intersection des deux requêtes,
et le Query DSL de ferrite sait déjà traduire la seconde. Les sous-agrégations
tournent sur ce croisement — la définition même de l'agrégation chez Elastic.
N'importe quelle clause que ferrite sait traduire est donc utilisable comme
filtre, et le résultat est mesuré identique à ES (11 cas dans `diff_aggs.py`).

Sous une agrégation de **buckets** (`terms` → `filter`), elle reste refusée
explicitement : il faudrait rejouer sa requête bucket par bucket, ce qui n'est
pas la même mécanique. Au premier niveau, et sous une autre `filter`, elle
fonctionne.

**Quatre écarts avec tantivy sont corrigés au passage** — ils sont la raison
d'être de la couche de mise en forme dans `src/aggs.rs` :

1. tantivy compte les dates en **nanosecondes**, ES en millisecondes ;
2. ES ajoute un `*_as_string` à côté de chaque métrique de date ;
3. ES départage les buckets `terms` **ex æquo par clé croissante**, pas tantivy —
   ce qui changeait non seulement l'ordre mais la **sélection** au bord de la
   troncature. ferrite demande donc 500 buckets de plus que la `size` voulue,
   applique l'ordre d'ES, puis tronque. Au-delà de 500 termes à égalité sur la
   frontière, la sélection pourrait encore différer ;
4. ES formate les bornes d'un `range` en flottants (`*-100.0`), même sur un champ
   entier, et rend la clé d'un `date_histogram` en entier.

### `date_histogram` : le calendrier et le fuseau

Un mois n'est pas trente jours, et un jour n'est pas toujours vingt-quatre
heures. `fixed_interval` ne sait dire ni l'un ni l'autre, et c'est pour ça que
`calendar_interval` était refusé : tantivy n'a pas d'équivalent du mois civil.
La carte pariait que ce refus n'était pas nécessaire — **le seau d'une date est
une fonction pure du calendrier**, que ferrite peut appliquer lui-même, comme il
exécute déjà l'agrégation `filter`. Le pari tient, à une correction près qui a
décidé de toute la mécanique : ce qu'il faut calculer soi-même, ce n'est pas un
seau à la fois, c'est la **liste des bornes**. Une fois cette liste connue,
l'agrégation redevient un `range` contigu — que tantivy exécute, avec ses
sous-agrégations, ses seaux vides et sa fusion multi-index.

D'où trois temps ([`src/histodate.rs`](../src/histodate.rs)) : une **pré-passe**
mesure le premier et le dernier instant du champ sur la même requête (c'est
exactement ce qu'ES connaît au moment de remplir les trous), les bornes sont
déroulées par l'arrondi, puis le résultat est remis en forme de
`date_histogram`.

**L'arrondi est celui d'Elasticsearch, pas une idée de l'arrondi calendaire.**
`org.elasticsearch.common.Rounding` est reproduit dans
[`src/calendrier.rs`](../src/calendrier.rs), et l'oracle n'est pas une lecture :
`tests/compat/genere_fuseaux.py --grille` fait tourner **cette classe-là**, dans
le conteneur de référence avec les jars d'ES au classpath, sur une grille de
25 914 arrondis (603 fuseaux × intervalles × instants choisis autour des
bascules de chaque zone) ; `tests/arrondi_vs_es.rs` la rejoue dans `cargo test`,
sans Docker. Rien de ce qui suit n'était devinable :

- une heure locale **qui n'existe pas** (le dimanche de mars) : pour un seau qui
  tombe à minuit, ES prend l'instant de la bascule — à Santiago, le seau du
  8 septembre 2024 commence à `01:00-03:00` ; pour un seau plus court, il repart
  de juste avant le trou ;
- une heure locale qui existe **deux fois** (le dimanche d'octobre) n'est pas
  tranchée de la même façon selon que l'unité tombe à minuit ou non ;
- un `fixed_interval` avec un fuseau **n'est plus fixe** : un seau de 3 h posé
  sur la nuit du changement d'heure à Paris dure quatre heures réelles ;
- `hard_bounds` et `extended_bounds` sont lues dans le fuseau **puis arrondies**,
  et pas du même côté : la borne haute de `hard_bounds` est exclue, celle
  d'`extended_bounds` incluse.

Les règles de changement d'heure viennent du **tzdb du JDK d'Elasticsearch**
(`jdk/lib/tzdb.dat`, 603 zones, tzdb 2024a), dumpé du conteneur de référence par
`tests/compat/genere_fuseaux.py` et embarqué dans le binaire
([`src/tzdata.bin`](../src/tzdata.bin), 110 Ko). Une table tirée d'ailleurs
divergerait de l'arbitre sur toute zone dont les règles ont bougé entre deux
versions du tzdb — et elle divergerait en silence : un seau décalé d'une heure
ressemble à un seau.

Le tout est mesuré seau par seau, `key`, `key_as_string` et `doc_count`
compris, par [`sonde_calendrier.py`](../tests/compat/sonde_calendrier.py) sur un
corpus qui traverse les deux bascules de l'année, un 29 février et un minuit qui
n'existe pas.

**Ce que ça coûte**, parce qu'un chemin de plus n'est pas gratuit : sur 20 000
documents, un `date_histogram` passe de 1,4 à 2,5 ms en `fixed_interval: 30d`
(12 seaux), de 3,3 à 9,5 ms en `1d` (365 seaux) et de 15,6 à 28,9 ms avec une
sous-agrégation — deux binaires *release* sur la même machine, médiane de vingt
tours, un `terms` témoin inchangé. La pré-passe `min`/`max` est une lecture de
plus, et une agrégation `range` cherche son seau par dichotomie là où un
histogramme divise. C'est le prix d'une seule mécanique pour tous les
`date_histogram` — la même que celle qui rend les mois civils et les fuseaux
possibles.

### `nested`

Un `nested` conserve la correspondance entre les sous-champs d'un même élément :
« une ligne `vis` d'au moins 20 » ne remonte pas un document qui a une ligne
`vis` **et** une ligne de 20 sans que ce soit la même.

Il n'y a pas de document caché ni de jointure de bloc : chaque champ sous un
`nested` a une colonne jumelle qui retient, pour chaque valeur, **de quel
élément** elle vient. La requête interne sert de pré-filtre (postings), puis
chaque candidat est vérifié élément par élément sur les colonnes. Conception et
mesures : [`nested-join.md`](nested-join.md), `src/nested.rs`.

| | État | Détail |
|---|---|---|
| `{"nested": {"path", "query"}}` | ✅ | `path` doit être un champ déclaré `nested` |
| Clauses internes | 🟡 | le minimum s'y compte **par élément**, comme ES le compte par document caché. Supporté : `term`, `terms`, `range`, `exists`, `prefix`, `match_all`, `match_none`, `match` (sur un champ non analysé), `bool` (`must` / `filter` / `should` + [`minimum_should_match`](#minimum_should_match) dans ses quatre notations / `must_not`) |
| Champ `text` dans une clause interne | ❌ | **divergence de moteur** — les colonnes portent la valeur, pas les termes analysés : interroger son multi-field `.keyword`, ou sortir la clause du `nested` |
| `nested` dans un `nested` | ❌ | **pas encore** — il faudrait un indice d'élément par niveau |
| `score_mode` | 🟡 | le score est celui de la requête interne évaluée à plat, il n'y a pas de score par élément. Supporté : `none`, `avg`. Refusé : `max`, `min`, `sum` |
| `inner_hits`, `ignore_unmapped` | ❌ | **pas encore** — `inner_hits` demande de rendre l'élément qui a correspondu : ferrite le sait (c'est la colonne jumelle), il ne l'expose simplement pas |
| Champs devinés sous un `nested` | ✅ | le mapping dynamique fonctionne, et la corrélation avec |
| Tri et agrégations sur un sous-champ de `nested`, depuis la racine | ❌ | **divergence de moteur** — ils porteraient sur les valeurs à plat, donc sur autre chose que ce que la requête a filtré. Chez Elasticsearch ces valeurs vivent dans des documents cachés : une **agrégation** n'en voit aucune et rend son résultat vide (`null`, `0.0`, `buckets: []`), un **tri** est refusé (`it is mandatory to set the [nested] context on the nested sort field`). ferrite les porte sur le document parent : il rendait donc une moyenne de `7.0` là où ES rend `null`, et un ordre en 200 là où ES rend 400. Un chiffre plausible et faux est le pire des résultats — le refus est explicite, et c'est le même que pour la requête équivalente (divergence n° 10) |

### `join` (parent/enfant)

Parent et enfant sont deux documents distincts, réunis à la requête.
`has_child` / `has_parent` s'évaluent en **deux passes** : la requête interne est
exécutée, les identifiants qui en sortent deviennent une recherche sur `_id` (ou
sur la colonne du parent). Exact, et borné par le nombre d'identifiants
distincts. Elasticsearch a besoin de *global ordinals* pour ça parce qu'il est
distribué ; mono-shard, parent et enfant sont forcément au même endroit.

| | État | Détail |
|---|---|---|
| `{"type": "join", "relations": {...}}` | ✅ | un seul champ `join` par index, plusieurs relations et plusieurs enfants par parent |
| Indexation | ✅ | `"lien": "article"` ou `{"name": "commentaire", "parent": "a1"}`. Un enfant sans `parent`, un parent avec, ou une relation non déclarée : refus explicite |
| `has_child`, `has_parent` | 🟡 | avec n'importe quelle requête interne. Supporté : `type` (`parent_type` pour `has_parent`), `parent_type`, `query`, `score_mode` (`none` seulement — la jointure rend un score constant). Refusé : `min_children`, `max_children`, `inner_hits`, `ignore_unmapped`, `score` |
| `parent_id` | ✅ | |
| `{"term": {"lien": "article"}}` | ✅ | le champ `join` se filtre comme un `keyword`, sous son propre nom, comme chez ES |
| `routing` | 🟡 | accepté et sans objet : il n'y a qu'un shard, donc rien à co-localiser. C'est **une contrainte d'ES en moins** |

## Les petites routes qui débloquent un outil

Cinq routes sans difficulté de moteur, dont l'absence faisait échouer des outils
entiers : un outil de découverte de champs, un script d'init qui pose un
template, un tableau de bord qui lit `_stats`.

### `_field_caps` — ce que chaque champ sait faire

`GET|POST /{index}/_field_caps?fields=*` rend, par champ, son type, `searchable`
et `aggregatable`, et l'**agrégation par index** quand plusieurs sont visés :
c'est la question que pose un outil de découverte avant de proposer un filtre
qui échouerait sur la moitié des index. Toute l'information est déjà dans le
mapping — ferrite n'a ni `index: false` ni `doc_values: false`, donc les deux
drapeaux se déduisent du type (un `text` n'est pas agrégeable, un `object` et un
`nested` ne sont ni l'un ni l'autre, tout le reste est les deux ; mesuré contre
ES 8.15).

Une règle de la réponse d'ES n'était pas devinable et vient d'une mesure : la
liste `indices` n'apparaît sur une entrée de type que si le champ a **plusieurs**
entrées. Un champ présent dans un seul des deux index visés n'a donc pas de
`indices` tant qu'il n'a qu'un type — c'est `include_unmapped=true` qui, en
ajoutant l'entrée `unmapped`, le fait apparaître.

`index_filter` est supporté : il ne décrit que les index qui ont au moins un
document correspondant, ce qui évite de décrire mille index quotidiens quand un
seul porte la période demandée.

### `_validate/query` — la requête est-elle valide, et sinon pourquoi

C'est le traducteur du Query DSL rendu observable, sans exécuter la recherche.
Ce qu'il fallait reproduire, ce sont les **deux formes de réponse** d'ES, et la
distinction compte :

| Ce qui est invalide | Ce qu'ES rend |
|---|---|
| la requête elle-même (clause inconnue, paramètre refusé) | `{"valid": false}`, **sans** `_shards` — et `error` avec `explain=true` |
| la requête sur *ce mapping-là* (une valeur qui n'a pas le type du champ) | `_shards`, et une explication par index |

ferrite trouve la première au même endroit qu'ES : en construisant la requête
contre un **schéma vide** ([`engine::sans_index`](../src/engine.rs)), où aucune
erreur ne peut venir d'un mapping. Seules les erreurs de *forme* y comptent —
c'est le fuzzer différentiel qui l'a montré, en trouvant qu'un `nested` sur un
chemin absent échoue aussi contre un schéma vide et sortait `valid: false` là où
ES dit `true`.

### `_stats` — les compteurs que ferrite mesure

`GET /{index}/_stats` rend la forme d'ES — `_shards`, `_all`
(`primaries` / `total`), `indices` — et **quatre** groupes : `docs`, `store`,
`segments`, `shard_stats`. Sur un moteur mono-shard sans réplique, `primaries`
et `total` portent les mêmes nombres : c'est vrai, pas une simplification.

Les autres groupes (`indexing`, `search`, `get`, `merge`, `translog`, les
caches…) ne sont **pas** rendus à zéro. Un `index_total: 0` sur un index où l'on
vient d'écrire mille documents ferait passer « non mesuré » pour « aucune
activité » : c'est l'échec silencieux que ce projet interdit, et il est pire
qu'un refus. Un client qui en nomme un (`GET /_stats/indexing`) reçoit donc une
erreur explicite.

### `PUT /{index}/_settings` — les réglages inertes, plutôt qu'un script cassé

ferrite n'a qu'un réglage qui change ses réponses
(`index.query.parse.allow_unmapped_fields`), et il est figé à la création :
la route était refusée en bloc pour autant. Le prix était disproportionné —
un script d'init entier échouait sur un `number_of_replicas: 1` qui ne
changerait rien ici.

Les réglages **inertes** sont donc acceptés, gardés et rendus par
`GET /{index}/_settings` : ils décrivent déjà ce que ferrite est. Tout le reste
est refusé explicitement, `index.blocks.*` et `index.max_result_window` compris —
ceux-là changeraient le comportement, et les avaler serait le même échec
silencieux.

Un cas mérite d'être dit, parce qu'il n'est pas inerte : `index.refresh_interval`
n'est pas accepté-et-ignoré. La valeur `-1` sort **vraiment** l'index de la
boucle de rafraîchissement de fond (`POST /{index}/_refresh` continue de
marcher) ; une valeur positive est honorée au sens où ES la définit — « visible
au plus tard après ce délai » — puisque ferrite rafraîchit toutes les secondes.

`flat_settings` est appliqué là où ferrite rend des réglages d'index, et
`GET /_settings` sans index vaut `_all` ; `/{index}/_settings/{nom}` filtre par
nom de réglage (liste, jokers, `_all`), sur les clés **aplaties** — sans quoi le
même nom filtrerait autrement selon `flat_settings`.

### Templates d'index — les deux familles

`_index_template` (la forme actuelle) et `_template` (l'ancienne, dépréciée mais
toujours servie par ES 8) : `PUT`, `GET`, `HEAD`, `DELETE`, et l'application du
template à la création de l'index. Les deux, parce que c'est `_template` qu'on
trouve dans le script d'init d'un projet resté en 7.x — et le produit, c'est que
ce code-là ne change pas.

Le template s'applique à la création **implicite** (écrire dans un index absent)
comme **explicite** (`PUT /{index}`, où le corps de la requête l'emporte) — c'est
ce que fait ES, mesuré. Un composable qui correspond éclipse les anciens ; sinon
tous les anciens qui correspondent sont fusionnés par `order` croissant.

Le contenu est validé **à la pose**, pas à la création de l'index : un réglage
refusé, un type de champ inconnu ou un alias filtré font échouer le `PUT`, là où
le client regarde. Les découvrir six mois plus tard, au premier document écrit
dans `logs-2027.01.01`, serait la même information rendue inutilisable.

Deux composables de même priorité dont les motifs se recouvrent rendraient la
création ambiguë : ES refuse, ferrite aussi. Le recouvrement est une
approximation assumée — on ne calcule pas l'intersection de deux jokers, on
regarde si l'un décrit l'autre pris pour un nom — et elle ne peut que
**sur**-détecter, jamais laisser passer deux motifs identiques.

## Erreurs

Format identique à celui d'Elasticsearch :

```json
{"error": {"root_cause": [{"type": "...", "reason": "..."}],
           "type": "...", "reason": "..."},
 "status": 400}
```

Types réutilisés d'ES : `index_not_found_exception`,
`resource_already_exists_exception`, `invalid_index_name_exception`,
`illegal_argument_exception`, `parsing_exception`, `query_shard_exception`,
`document_parsing_exception`, `strict_dynamic_mapping_exception`,
`version_conflict_engine_exception`. Une route inconnue renvoie le 400
`no handler found for uri [...] and method [...]` d'ES.

---

## Divergences assumées avec Elasticsearch

Ce ne sont pas des manques, ce sont des choix — ils sont ici pour être discutés,
pas pour être découverts en production.

1. **Un champ inconnu dans une requête ne correspond à rien — sauf si on
   demande le contraire.** C'était la divergence assumée numéro un du projet :
   ferrite refusait la requête là où ES rend 0 hit, au motif que sans mapping
   dynamique un champ inconnu est toujours une faute de frappe.

   Un vrai client l'a démentie. Un filtre `archiveAt` posé sur **chaque**
   recherche, sur un jeu où aucune commande n'est encore archivée — donc où le
   champ n'est jamais mappé — faisait échouer l'application entière, en 400, là
   où ES répondait. Le raisonnement était juste dans l'absolu et faux en
   pratique : le champ inconnu n'est pas toujours une faute, c'est aussi un
   mapping qui n'est pas encore né.

   ferrite implémente donc le vrai réglage d'ES,
   `index.query.parse.allow_unmapped_fields`, avec **son** défaut (`true`) : la
   clause ne correspond à rien, et les clauses qui l'entourent continuent de
   compter (`must_not: exists` sur un champ non mappé matche donc tous les
   documents, comme chez ES — mesuré). Le mode strict reste disponible en posant
   le réglage à `false` dans les `settings` à la création de l'index ; l'erreur
   est alors le `query_shard_exception` d'ES, et elle nomme le réglage.

   Ça ne change rien à deux points voisins : un champ inconnu dans une
   **agrégation** reste une erreur (ES rend un bucket vide ; c'est la divergence
   n° 11), et un sous-champ de `nested` interrogé depuis la racine aussi
   (n° 10).

2. **`slop` est refusé dans `match_phrase`.** tantivy et Lucene ne comptent pas
   les déplacements de la même façon dès que la phrase dépasse deux termes :
   cherchée comme `un deux trois`, la phrase `deux un trois` correspond à
   `slop: 2` chez Elasticsearch et seulement à `slop: 3` chez tantivy. Accepter
   le paramètre ferait donc rendre à ferrite **moins de documents** qu'ES sur la
   même requête, sans que rien ne le signale. La phrase exacte (`slop` absent ou
   `0`) est vérifiée identique à ES.

3. **Quatre opérateurs de `regexp` sont refusés, pas ignorés.** La syntaxe de
   `regexp` est celle de Lucene ; ferrite la traduit vers celle du crate `regex`
   ([`src/regexp.rs`](../src/regexp.rs)), qui construit un automate incapable de
   complément (`~`), d'intersection (`&`), d'intervalle numérique (`<1-100>`) et
   de langage vide (`#`). Les prendre pour des caractères littéraux — ce que
   ferait un passage direct du motif — rendrait **d'autres documents** qu'ES sans
   que rien ne le signale : ils sont donc refusés explicitement. Le paramètre
   `flags` d'ES les désactive (`"flags": "NONE"`), et ils redeviennent alors des
   caractères littéraux des deux côtés.

   Tout le reste est traduit et **mesuré identique** à ES 8.15 par
   [`tests/compat/diff_motifs.py`](../tests/compat/diff_motifs.py), y compris ce
   que la ressemblance des deux syntaxes fait rater : le motif est ancré des deux
   côtés (`^` et `$` ne sont pas des ancres mais des caractères), `@` veut dire
   « n'importe quelle chaîne » (le piège du motif d'adresse e-mail), `"abc"` est
   une chaîne littérale, les classes prédéfinies (`\d`, `\w`, `\s`…) sont
   **ASCII** là où celles de `regex` sont Unicode, et `case_insensitive` ne
   replie que l'ASCII, et seulement les caractères isolés — `[d-e]` n'y matche
   pas `D`, chez ES comme ici.

   Et un bord de plus, corrigé depuis : **le `|` de Lucene n'a pas de branche
   vide.** Son analyseur lit toujours un atome après un `|`, et rend un
   caractère **littéral** devant tout ce qu'il ne reconnaît pas — `|` compris.
   Donc `|a` cherche la chaîne `|a`, `a||b` cherche `a` ou `|b`, et `a|` échoue
   en 400 (`unexpected end-of-string`). ferrite en faisait de vraies
   alternations à branche vide : `|a` rendait les documents vides **et** ceux
   qui portent `a`, en 200. Trouvé par une plage de graines neuves du fuzzer,
   figé dans `diff_motifs.py`.

4. **`best_fields` n'utilise pas le `DisjunctionMaxQuery` de tantivy.**
   Dans tantivy 0.26 cette requête rend la **somme** des scores et non leur
   maximum, quel que soit le `tie_breaker` (le combineur est court-circuité par
   une spécialisation interne, et le constructeur correct est `pub(crate)`).
   S'en servir donnerait silencieusement un classement `most_fields` à qui
   demande `best_fields`. ferrite implémente donc `dis_max` lui-même dans
   `src/dismax.rs`, en déléguant le parcours des documents à tantivy et en ne
   recalculant que le score. Un test unitaire verrouille « max, pas somme » pour
   qu'une montée de version ne puisse pas dégrader la pertinence en silence.

5. **Analyse du texte.** Les champs `text` utilisent le tokenizer `default` de
   tantivy (découpe sur les non-alphanumériques + minuscules + rejet des tokens
   de plus de 40 caractères). Très proche de l'analyzer `standard` d'ES pour du
   texte latin, mais ce n'est pas la même implémentation : sur de l'unicode
   exotique ou du CJK, les tokens peuvent différer.

6. **Les scores ne sont pas identiques à ceux d'ES.** Même formule (BM25), mais
   statistiques d'index et normalisation de longueur différentes. L'*ordre* des
   résultats est comparé à celui d'ES par `tests/compat/diff_against_es.py` ;
   les valeurs absolues, non.

7. **`_shards.total` vaut 1** (un shard, zéro réplique) là où un ES par défaut
   annonce 2 dans les réponses d'écriture. En recherche multi-index, il vaut le
   **nombre d'index visés** : un index = un shard, et c'est ce que compte ES.

8. **`_cluster/health` est toujours `green`.** C'est le comportement voulu pour
   un mono-nœud : il n'y a pas de réplique à assigner.

9. **`wait_for` vaut `true` pour `refresh`.** Le commit est synchrone, il n'y a
   rien à attendre.

10. **Un sous-champ de `nested` interrogé depuis la racine est une erreur, pas 0
    résultat.** Chez Elasticsearch, ces valeurs vivent dans des documents cachés :
   `{"term": {"lignes.ref": "vis"}}` hors d'une clause `nested` ne rend **rien**,
   en silence — un piège classique. ferrite les indexe sur le document parent, il
   pourrait donc y répondre, et rendrait alors des documents là où ES n'en rend
   aucun. Il refuse explicitement, en nommant la clause `nested` attendue.

    La règle vaut aussi pour ce qui **lit** ces valeurs sans les filtrer, et
    c'est là qu'elle manquait. Une **agrégation** sur `lignes.prix` posée depuis
    la racine ne voit chez ES aucun document : il rend `null`, `0.0` ou
    `buckets: []` selon l'agrégation. ferrite, lui, agrégeait à plat — mesuré :
    une moyenne de `7.0` là où ES rend `null`, une somme de `21.0` là où ES rend
    `0.0`. Un **tri** sur le même chemin est carrément refusé par ES
    (`it is mandatory to set the [nested] context on the nested sort field`) là
    où ferrite rendait un ordre en 200. Les deux sont maintenant refusés, pour
    la raison qui vaut dans tout ce dépôt : un chiffre plausible et faux est
    pire qu'une erreur. Rendre le résultat vide d'ES serait une autre option,
    mais elle demanderait de savoir agréger *dans* le contexte `nested` pour ne
    pas se contenter d'annoncer zéro — l'agrégation `nested` n'est pas encore
    supportée, et ce zéro-là est justement le piège qu'ES tend à ses clients.

11. **Un champ inconnu dans une agrégation reste une erreur.** ES rend un
    résultat vide (`buckets: []`, `value: null`, `sum: 0.0` selon l'agrégation) ;
    ferrite refuse, en nommant le champ. Contrairement au cas de la requête
    (divergence n° 1), aucun usage réel n'a encore montré qu'une agrégation
    portait sur un champ pas encore mappé — et `allow_unmapped_fields` ne
    gouverne pas ce cas chez ES non plus. En multi-index, la règle est la même
    que pour les requêtes : si un *autre* index visé mappe le champ, l'index qui
    l'ignore n'agrège simplement pas.

12. **Une recherche qui ne vise aucun index refuse quand même ce que ferrite ne
    sait pas faire.** Sur un cluster vide (ou un motif qui ne correspond à rien),
    `{"aggs": {"a": {"significant_terms": …}}}` et `{"query": {"intervals": …}}`
    rendent **400** ici et **200** chez ES — non parce qu'ES les ignore, mais
    parce qu'il *sait* les faire : son 200 est une vraie réponse vide, le nôtre
    serait un silence. La règle qui prime dans ce dépôt tranche : un client qui
    écrit ça contre un cluster vide doit l'apprendre tout de suite, pas le jour
    où il aura des données.

    La frontière est mesurée, pas devinée
    ([`tests/compat/sonde_vide.py`](../tests/compat/sonde_vide.py), 28/28
    identiques) : sur tout ce qu'ES lui-même refuse sans index — une clause
    inconnue, un type d'agrégation inconnu, une clé de corps inconnue, un ordre
    de tri invalide — les deux serveurs rendent le même statut. Et sur ce qu'ES
    diffère à l'exécution d'un shard — un champ non mappé dans un `term`, un
    `sort`, une agrégation, un chemin `nested` inexistant — les deux rendent 200
    et le **même corps**, `max_score: 0.0` et absence de section `aggregations`
    comprises. Sans shard, il n'y a pas de verdict de mapping à rendre.

13. **`_field_caps` n'expose pas les champs de métadonnées.** Sur `fields=*`, ES
    décrit aussi `_id`, `_index`, `_seq_no`, `_source`, `_routing`, `_tier` et
    une dizaine d'autres. ferrite ne les rend pas : il ne sait pas les
    interroger, et les annoncer `searchable: true` serait un résultat faux — un
    outil qui construirait un filtre dessus n'obtiendrait rien, en silence. Les
    champs du mapping, eux, sont mesurés identiques à ES par le fuzzer
    différentiel, sur des mappings tirés au sort.

14. **L'`explanation` de `_validate/query` est celle de ferrite.** Celle d'ES est
    la chaîne Lucene de la requête réécrite ; ferrite rend le rendu de la requête
    tantivy qu'il a construite (avec les noms de champ remis à la place des
    numéros internes). Les deux moteurs ne construisent pas les mêmes objets, et
    inventer une chaîne Lucene qu'on n'a pas serait pire que d'en rendre une qui
    dit honnêtement ce que ferrite a compris. Ce qui doit coïncider — et qui est
    comparé par le fuzzer sur chaque requête tirée au sort — c'est le **verdict**
    `valid`. `rewrite=true`, qui demande explicitement la forme réécrite, est
    refusé.

15. **`_stats` ne rend que les groupes que ferrite mesure**, et
    `docs.count` n'y compte pas la même chose qu'ES dès qu'il y a du `nested` :
    Lucene indexe chaque élément d'un tableau `nested` comme un document à part
    et les compte, ferrite n'a pas ces sous-documents (voir
    [`nested-join.md`](nested-join.md)) et compte ce qu'il a. Aucun des deux ne
    ment. Le fuzzer ne le tolère pas en bloc : il exige que le compte de ferrite
    égale ce que la recherche rend des deux côtés, et que celui d'ES lui soit
    strictement supérieur.

16. **ferrite ne rend pas `_ignored`.** ES pose dans chaque hit la liste des
    champs qu'un `ignore_above` (ou un `ignore_malformed`, que ferrite n'a pas)
    a écartés à l'indexation, et il l'expose aussi comme champ adressable dans
    `fields`. ferrite ne tient pas cette liste : la rendre vide dirait « aucun
    champ écarté » alors qu'on ne le sait pas, donc `fields: ["_ignored"]` est
    **refusé explicitement** plutôt que rendu vide. Ce que ferrite rend, en
    revanche, c'est `ignored_field_values` — le bloc frère qui porte les
    **valeurs** écartées, et qui ne sort qu'avec `fields`, donc là où le client
    les a demandées. C'est ce qui fait qu'une valeur trop longue pour
    `ignore_above` ne se retrouve **pas** dans `fields` : elle n'a pas été
    indexée, ES ne la rend pas là, et ferrite non plus depuis qu'on l'a mesuré
    ([`sonde_fields.py`](../tests/compat/sonde_fields.py)).

17. **`docvalue_fields` ne rend rien sous un `nested`.** Chez ES, les valeurs
    d'un sous-champ de `nested` vivent dans des documents cachés : il n'en voit
    aucune et ne rend pas de clé. ferrite les porte, lui, sur le document parent
    (voir [`nested-join.md`](nested-join.md)) — les rendre serait rendre **plus**
    qu'ES, en silence. Il les écarte donc explicitement. `fields`, lui, les rend
    des deux côtés, groupées par élément : c'est la lecture du `_source`, et le
    `_source` porte bien le tableau.

18. **L'ordre des valeurs qu'un `copy_to` dépose dans sa cible.** Sur un
    `fields` posé sur la cible, les deux serveurs rendent **les mêmes valeurs**,
    dans un ordre différent dès qu'il y a plus d'une source. Celui d'ES n'est
    pas un ordre : c'est l'itération d'un `HashSet<String>` de Java sur
    l'ensemble {cible} ∪ {sources}, donc des seaux de hachage. La mesure suffit
    à l'établir — trois sources `aa`, `mm`, `zz` en ressortent triées, mais
    `tag` en ressort **avant** `client.ville`, ce qu'aucun tri ne donne. ferrite
    rend un ordre qu'on peut écrire : la valeur propre de la cible d'abord, puis
    les sources par ordre de nom. Le prédicat de
    [`sonde_fields.py`](../tests/compat/sonde_fields.py) **mesure** que l'écart
    ne porte que sur l'ordre — une valeur en trop ou un doublon perdu y reste un
    écart.

    Le **surlignage** de la cible hérite du même désordre, et il ne se rattrape
    pas de la même façon : il ne rend pas toutes les valeurs, il en **choisit**
    (`no_match_size` prend la première, `number_of_fragments` garde les mieux
    notées) — et « la première » n'existe pas quand l'ordre vient d'un
    `HashSet`. Deux Elasticsearch de la même version n'y rendent déjà pas la
    même chose : `fuzz_vs_es.py --calibrer` le montre.

19. **Un fragment de surlignage se compte en `char`, pas en unité UTF-16.**
    `fragment_size` et `no_match_size` sont des longueurs, et Java les compte en
    unités UTF-16 — deux par caractère au-delà du plan multilingue de base
    (émojis, écritures anciennes). ferrite les compte en `char`. Sur du texte
    ordinaire, accents compris, les deux coïncident ; ils divergent d'un
    caractère par émoji présent **avant** le point de coupe, et seulement là.

20. **Une erreur de lecture du corps ne porte pas de position.** ES préfixe ses
    `x_content_parse_exception` par la ligne et la colonne fautives
    (`[1:82] [highlight] unknown field [nawak]`) ; ferrite rend la même phrase
    sans le préfixe. Il ne tient pas de position de lecture — son analyseur
    JSON rend un arbre, pas un flux de jetons — et inventer une position serait
    pire que ne pas en donner.

21. **Les trois lectures sur le même champ stocké rendent un `500` chez ES.**
    `{"fields": ["tag"], "docvalue_fields": ["tag"], "stored_fields": ["tag"]}`
    sur un `keyword` déclaré `store: true` fait rendre à ES 8.15 un
    `unsupported_operation_exception`. Un 500 ne se reproduit pas — c'est déjà
    la raison pour laquelle `_seq_no` nommé dans `fields` est refusé ici.
    ferrite rend les valeurs, comme il le fait pour chacune des trois prises
    séparément.

22. **Une somme qui n'est pas finie est rendue `null`, pas `"Infinity"`.**
    Au-delà de 1,8 × 10³⁰⁸ une somme de `double` déborde. ES rend alors une
    **chaîne JSON** dans un champ numérique — mesuré sur un document dont le
    champ vaut `[1e308, 1e308]` : `{"sum": {"value": "Infinity"}}`,
    `{"avg": {"value": "Infinity"}}`, et `"-Infinity"` dans l'autre sens.
    ferrite rend `{"value": null}`.

    Deux raisons, et la seconde est la vraie. La première est que
    `serde_json` écrit `null` pour tout flottant non fini, et qu'un
    sérialiseur qui distinguerait `null` d'un infini est un `Serializer` complet
    à écrire — la valeur est déjà perdue quand ferrite la reçoit. La seconde est
    que la parité ne serait de toute façon **pas** atteinte : ES et tantivy ne
    débordent pas pareil. ES arrête de compenser dès que son accumulateur cesse
    d'être fini et garde `Infinity` ; la compensation de Kahan de tantivy, elle,
    calcule `(inf − inf)` au coup d'après et devient `NaN` pour de bon. Sur deux
    documents valant `[1e308, 1e308]` et `[-1e308, -1e308]`, ES rend donc
    `"Infinity"` et ferrite `NaN` — rendre `"NaN"` au lieu de `null` aurait
    remplacé un mensonge par un autre.

    Ce qui **n'est pas** dans cette divergence, et c'est l'essentiel : tant que
    la somme reste finie, les deux moteurs rendent la même valeur, ordre des
    valeurs d'un document multivalué compris. C'est ce que la section suivante
    mesure.

    Elle ne couvre pas non plus le tableau **`sort`**, qui a le même problème et
    une autre réponse. Une valeur de tri absente sur un flottant *est*
    `Infinity` chez ES, et une somme (`mode: sum`) qui déborde l'est aussi : les
    deux y sortent en **chaîne**. ferrite les rend donc en chaîne, et pas `null`
    — la valeur n'y est pas perdue avant d'arriver, contrairement à
    l'accumulateur d'une agrégation, et un `sort` que le client renvoie tel quel
    doit se relire.

23. **Le chemin d'ordre d'un `terms` est vérifié même quand il n'y a rien à
    trier.** ES ne le résout qu'au moment de comparer deux seaux : avec zéro ou
    un seul seau, il ne trie rien et **ne valide rien**. Mesuré contre ES 8.15
    sur le même index et la même agrégation, en faisant varier le seul nombre de
    seaux retenus par un `include` :

    | seaux retenus | `order: {stats_sans_clé: "desc"}` |
    |---|---|
    | 8 | 400, `Missing value key in [null]` |
    | 2 | 400, le même |
    | 1 | **200** |
    | 0 | **200** |

    Ce n'est pas propre à cette faute-là : une agrégation d'ordre qui n'existe
    pas, une agrégation de seaux prise comme clé, une propriété que la métrique
    ne rend pas — les trois passent aussi en 200 dès qu'il ne reste qu'un seau.
    Et `size: 1` ne suffit pas : ES collecte les huit seaux et ne tronque
    qu'après, donc il compare bien et il refuse.

    ferrite valide la demande **avant** de l'exécuter, comme partout ailleurs.
    Faire dépendre la validation du nombre de documents trouvés rendrait la même
    requête tantôt acceptée tantôt refusée, et un client qui teste sur un jeu
    vide découvrirait le refus en production. Partout où ES a deux seaux à
    comparer, les cinq refus de chemin d'ordre sont mesurés **identiques aux
    siens** ([`sonde_facettes.py`](../tests/compat/sonde_facettes.py)).

### L'ordre dans lequel une agrégation lit les valeurs d'un document

Ce n'est pas une divergence — c'est une décision, et elle est ici parce qu'elle
n'était pas devinable et qu'elle a été prise **avec la mesure en main**.

`sum`, `avg` et `stats` accumulent en `double`, des deux côtés, avec la **même**
compensation de Kahan. Ce qui les séparait n'était donc ni le type de
l'accumulateur ni la formule, mais l'**ordre** : Lucene stocke les valeurs d'un
champ numérique multivalué **triées croissantes**
(`SortedNumericDocValues`), tantivy les garde dans l'ordre du document. Au-delà
de 2^53 un `double` ne représente plus tous les entiers, et l'ordre décide alors
du résultat.

Les valeurs, mesurées contre un ES 8.15 sur **un seul** document :

| Le document | ES 8.15 | ferrite, avant | ferrite, après |
|---|---|---|---|
| `{"v": [-2^63, 2^63-1, -1, -1]}` | `sum: 0.0` | `sum: 0.0` | `sum: 0.0` |
| `{"v": [2^63-1, -1, -2^63, -1]}` | `sum: 0.0` | `sum: -1.0` | `sum: 0.0` |
| `{"v": [2^63-1, -2^63, -1, -1]}` | `sum: 0.0` | `sum: -2.0` | `sum: 0.0` |
| `{"f": [1e308, 1e308, -1e308]}` | `sum: 1.0E308` | `value: null` (`NaN`) | `sum: 1e308` |

Le même contenu écrit **trié** s'accordait déjà des deux côtés : c'est le
désordre, et lui seul, qui séparait les deux moteurs. L'ordre des **documents**,
lui, n'a jamais divergé — mesuré sur douze corpus de 3 à 600 documents tirés au
sort, 0 écart.

Trois issues étaient possibles, et la mesure les a réduites à une seule.
Reproduire l'accumulation d'ES aurait été un choix s'il avait fallu perdre de la
précision pour l'obtenir — ce n'est pas le cas : trier la colonne rend ferrite
identique à ES **et** arithmétiquement meilleur (dans le dernier cas du tableau,
`NaN` devient `1e308`). Garder l'ordre du document et déclarer la divergence
aurait laissé un résultat faux en 200. Et refuser au-delà d'un seuil aurait
refusé une agrégation banale sur la foi d'une valeur.

La colonne est donc triée **à l'indexation**, là où Lucene la trie
([`src/engine.rs`](../src/engine.rs), `pose`) — c'est le seul endroit que
l'agrégation de tantivy, qui fait la somme elle-même, regarde. Deux conséquences
qu'il a fallu payer :

- un champ **stocké** (`store: true`) garde l'ordre du document, parce que c'est
  celui qu'ES rend à `stored_fields`. Chez Lucene un champ stocké et une colonne
  sont deux structures distinctes ; ferrite les confondait en un seul champ
  tantivy, et le tri a fait passer au rouge le cas figé qui l'exige (`[3, 1, 1]`
  devenait `[1, 1, 3]`). Un champ numérique `store: true` a donc désormais un
  champ jumeau `_store.{chemin}` ;
- la colonne jumelle `_elem.{chemin}` d'un `nested` suit sa valeur : le tri
  déplace la **paire**, sans quoi l'appariement positionnel dont dépend
  [`nested-join.md`](nested-join.md) serait rompu.

`fields`, lui, ne bouge pas : il lit le `_source`, donc l'ordre du document et
ses doublons — chez ES aussi.

## Limites connues (perf, pas fonctionnalité)

Ces limites ne sont plus seulement décrites : elles sont **mesurées**, sur un
corpus public et à deux échelles, par
[`bench_echelle.py`](../tests/compat/bench_echelle.py). Les chiffres et le
protocole sont dans [`bench.md`](bench.md) ; ce qui suit en donne la lecture.

**Jusqu'où ferrite est le bon choix.** Jusqu'à quelques millions de documents,
pour une charge faite de recherches filtrées qui ramènent peu : à deux millions
de documents de la track Rally `geonames`, un `term` y coûte 1,55 ms contre
2,58 ms à Elasticsearch, une `match_phrase` 1,20 ms contre 3,13 ms, et le
serveur tient dans 425 Mo de RSS contre 3,40 Go. **Le mauvais choix** dès que la
charge trie de gros résultats (jusqu'à ×290 plus lent, voir juste en dessous),
exporte en masse (`scroll` ×0,25), ou réindexe souvent (indexation ×0,20 aux
deux échelles). Rien n'a été mesuré au-delà de deux millions de documents, et
rien n'est extrapolé ici.

- ~~**Une sous-agrégation sous un `terms` ou un `range` perd les documents de
  ses buckets rares.**~~ **Corrigé.** C'était la seule limite de cette liste à
  rendre des **valeurs fausses en 200** : les `doc_count` des buckets étaient
  exacts, seules les valeurs des sous-agrégations manquaient, donc rien ne
  prévenait. Sur deux millions de documents de la track `geonames`, un `range`
  dont le bucket compte 28 518 documents rendait un `value_count` de 1 692 —
  94 % de perdus. La cause était dans tantivy 0.26.1
  (`aggregation/cached_sub_aggs.rs`, `LowCardSubAggCache::flush_local`), et la
  décision est prise sur des bornes **mesurées, pas lues dans son code** :
  2 047 documents dans un segment sont justes et **2 048** ne le sont plus ; un
  bucket est perdu s'il a au plus `2048 / (2 × nombre de buckets)` documents
  dans la fenêtre qui se vide (204 perdus, 205 gardés sur 5 buckets) ; et
  **toutes** les métriques étaient touchées, pas seulement `value_count` — un
  `avg` rendait 21,5 là où ES rend 21,428…, un nombre faux *plausible*. ferrite
  **épingle** le correctif d'amont ([tantivy#2992](https://github.com/quickwit-oss/tantivy/issues/2992),
  non publié : 0.26.1 reste la dernière version) ; ce que l'épingle contient et
  comment en sortir sont dans [`tantivy-patch.md`](tantivy-patch.md). La mesure
  qui le tient : [`sonde_sous_aggs.py`](../tests/compat/sonde_sous_aggs.py),
  46 combinaisons parent × sous-agrégation sur 50 000 documents — **46/46
  identiques à ES avec l'épingle, 32/46 sans**.
- **Le tri charge tous les hits en mémoire.** Le collecteur de tri ramasse tous
  les documents correspondants avec leurs clés avant de les ordonner. C'est
  correct pour toutes les combinaisons de clés (y compris `keyword` et
  multi-clés, où un tri par ordinal de terme serait faux entre segments), mais
  l'occupation mémoire — et le temps — sont proportionnels au nombre de
  documents **correspondants**, pas à `size`. La mesure donne l'ordre de
  grandeur : un `match_all` trié sur un entier coûte 170 ms sur 500 000
  documents et 727 ms sur 2 000 000, quand Elasticsearch reste entre 2,9 et
  12 ms aux deux échelles. C'est le pire résultat du banc, jusqu'à ×290. La
  recherche
  **sans** tri utilise un top-K classique et n'a pas cette limite.
- **Un `terms` à des dizaines de milliers de valeurs coûte dix fois plus cher
  qu'à Lucene** : les 45 587 termes des trois requêtes `large_terms` de la
  track prennent 1 023 ms sur 2 000 000 de documents, contre 126 ms à
  Elasticsearch.
- **L'indexation est cinq fois plus lente**, aux deux échelles : 11 298 doc/s
  contre 58 736 à 500 000 documents, 10 198 contre 51 484 à 2 000 000.
- **Un contexte de `scroll` tient toute la liste des correspondances en
  mémoire** (une adresse et ses clés de tri par document), plus l'instantané de
  l'index. C'est le prix de « chaque document une fois, et une seule, en un seul
  balayage » ; l'alternative — rejouer la requête à chaque page — coûterait N
  recherches pour N pages et ne figerait rien. Les contextes expirés sont purgés
  toutes les 30 s, et 500 au plus peuvent être ouverts. Le prix mesuré : 25
  pages de 1 000 documents coûtent 1 737 ms sur 2 000 000 de documents, contre
  433 ms à Elasticsearch — la première page paie pour les vingt-cinq.
- **`GET /{index}/_doc/{id}` déclenche un commit** si des écritures sont en
  attente, pour rester temps réel comme ES. Sous forte charge d'écriture, un
  `get` peut donc coûter cher.
- **La table `_id → (_version, _seq_no)` est en mémoire** et reconstruite au
  démarrage en relisant les fast fields de l'index. Coût proportionnel au
  nombre de documents au démarrage.
