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

**La suite de conformance d'Elasticsearch elle-même** (`tests/compat/conformance_es.py`,
la 7.10.2 — la dernière version Apache 2.0) donne l'état d'ensemble. Ses chiffres
ne sont pas recopiés ici : ils vivent dans [`conformance.json`](conformance.json),
régénéré par le runner et commité (l'étalonnage du runner contre un vrai
Elasticsearch 7.10.2 est dans [`conformance-es7102.json`](conformance-es7102.json)).

```bash
python3 -c "import json; print(json.load(open('docs/conformance.json'))['totaux'])"
```

La colonne « échecs » de ce rapport est l'inventaire des écarts qui restent — les
familles sont listées dans [`conformance.md`](conformance.md), avec de quoi les
compter soi-même. C'est la mesure la moins complaisante du projet : les cas
viennent d'Elastic, pas de nous. La CI en fait un cliquet : le nombre d'échecs
ne remonte pas.

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
| `highlight`, `search_after`, `pit`, `collapse`, `knn`, `explain`, `seq_no_primary_term`, `post_filter`, `min_score`, `suggest`, `rescore`, `track_scores`, `q`, `timeout`, `terminate_after`, `version`, `indices_boost`, `profile`, `slice`, `stats`, `ext`, `retriever` | 12,0 % | ❌ |
| `range` | 11,4 % | 🟡 |
| `PUT /{index}` | 10,5 % | 🟡 |
| Sous-agrégations | 8,8 % | ✅ |
| `terms` | 7,5 % | 🟡 |
| `match` | 6,8 % | 🟡 |
| `PUT\|POST /{index}/_doc/{id}` | 6,8 % | 🟡 |
| `query_string`, `simple_query_string`, `function_score`, `boosting`, `intervals`, `terms_set`, `script`… | 6,5 % | ❌ |
| `percentiles`, `extended_stats`, `top_hits`, `composite`, `filters`, `nested`, `significant_terms`, `date_range`, `ip_range`… | 6,1 % | ❌ |
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
| `GET /_nodes`, `/_nodes/{spec}` | 🟡 | un nœud, champs d'identité et `http` ; pas de `settings`, `os`, `jvm`. `{spec}` accepte `_all`, `_local`, `_master` et l'identifiant du nœud. Refusé : les sous-ressources (`_nodes/stats`, `_nodes/hot_threads`…) |

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
| `PUT /{index}` | 🟡 | `mappings` est **optionnel** (les champs viendront des documents) ; `settings` s'écrit à plat comme imbriqué. Supporté : `mappings`, `aliases`, `settings.number_of_shards` (accepté, sans effet : ferrite est mono-shard), `settings.number_of_replicas` (accepté, sans effet), `settings.index.query.parse.allow_unmapped_fields` (voir plus bas). Refusé : tout autre réglage (refusé plutôt qu'ignoré) |
| `DELETE /{index}` | ✅ | listes et motifs, sous `action.destructive_requires_name` (voir plus bas). `ignore_unavailable` honoré |
| `HEAD /{index}` | ✅ | 200 dès que l'expression se résout, même sur zéro index — comme ES |
| `GET /{index}` | ✅ | `aliases` / `mappings` / `settings`, une entrée par index visé |
| `GET /{index}/_mapping` | ✅ | |
| `GET /{index}/_mapping/field/{champs}` | ❌ | **pas encore** — route absente (`no handler found`) : ferrite a pourtant le mapping, c'est un manque et pas une impossibilité — 15 cas de la suite d'Elastic tombent dessus |
| `GET /{index}/_settings` | 🟡 | les réglages d'ES qu'un index a vraiment (`number_of_shards`, `uuid`, `creation_date`…), et `index.query.parse.allow_unmapped_fields` s'il a été posé. Supporté : `GET /_settings` sans index (vaut `_all`), `/{index}/_settings/{nom}` (filtrer par nom de réglage — liste, jokers, `_all`. Le filtre porte sur les clés **aplaties**, sans quoi le même nom filtrerait autrement selon `flat_settings`), `flat_settings` (il aplatit les clés (`index.number_of_shards`) ; longtemps refusé, parce qu'accepté et ignoré il rendait une réponse que personne n'avait demandée — c'est une réécriture de clés, elle est maintenant faite), `local` (un seul nœud : la question ne se pose pas). Refusé : `include_defaults` (il ajoute une section `defaults` avec les dizaines de réglages qu'ES a et que ferrite n'a pas) |
| `PUT /{index}/_settings` | 🟡 | les réglages **inertes** sont acceptés, gardés et rendus par `GET /{index}/_settings` : ils décrivent déjà ce que ferrite est (mono-shard, sans réplique). Faire échouer un script d'init entier sur un `number_of_replicas: 1` qui ne changerait rien serait pire que de l'accepter. `index.refresh_interval` n'est pas accepté-et-ignoré : `-1` sort vraiment l'index de la boucle de rafraîchissement de fond. Supporté : `number_of_replicas` (sans effet — ferrite n'a pas de réplique), `auto_expand_replicas` (sans effet, même raison), `refresh_interval` (`-1` désactive vraiment le rafraîchissement de fond ; une valeur positive est honorée, ferrite rafraîchissant toutes les secondes), `preserve_existing`, une valeur `null` (efface le réglage, comme chez ES). Refusé : `number_of_shards` (figé à la création, comme chez ES (`Can't update non dynamic settings`)), `index.query.parse.allow_unmapped_fields` (figé dans la génération courante du schéma ; un client qui le croirait changé chercherait longtemps), `reopen`, tout autre réglage d'ES (`index.blocks.*`, `index.max_result_window`… : les accepter puis les ignorer changerait le comportement en silence) |
| `PUT /{index}/_mapping` | 🟡 | **ajoute** des champs (une nouvelle génération est construite). Changer le type d'un champ existant reste refusé, comme chez ES. Refusé : `dynamic` (le modifier après coup) |
| `POST /{index}/_refresh` | ✅ | |
| `POST\|GET /_analyze`, `/{index}/_analyze` | 🟡 | Supporté : `text` (chaîne ou liste), `analyzer`, `field`. Refusé : `tokenizer`, `filter`, `char_filter` |
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
**210 textes** français et anglais (`tests/compat/diff_analyzers.py`) : des
phrases, et surtout un vocabulaire qui balaie les familles de suffixes.

| Analyzer | État | Détail |
|---|---|---|
| `standard` (défaut) | ✅ | identique à ES sur les 210 textes |
| `simple` | ✅ | identique |
| `whitespace` | ✅ | identique |
| `keyword` | ✅ | identique |
| `stop` | ✅ | identique |
| `english` | ✅ | identique — Porter porté depuis Lucene, filtre possessif compris |
| `french` | ✅ | identique — stemmer léger de Savoy, élision, mots vides relevés |
| `german`, `spanish`, `snowball` et les autres langues | ❌ | **divergence de moteur** — leur stemmer n'est pas porté, et livrer sous le nom d'ES un analyzer qui indexe autre chose changerait silencieusement les résultats d'un mapping existant |
| Analyzers sur mesure (`settings.analysis`) | ✅ | voir ci-dessous |

**Les stemmers de Lucene sont portés** (`src/stemmer.rs`) : le stemmer Porter
pour l'anglais, le stemmer léger de Savoy pour le français. Celui de tantivy
(Snowball) n'est celui d'aucun des deux — c'est ce qui donnait, avant ce
portage, **19 textes divergents sur 28 en `english` et 17 en `french`**.

**Les deux sont désormais identiques à ES sur les 210 textes.** `english` :
Porter (validé en plus sur les 66 exemples de l'article de Porter lui-même),
filtre possessif (`Peter's` → `Peter`), mots vides et ordre des filtres de
`EnglishAnalyzer`. `french` : stemmer léger de Savoy, élision (`l'ascension` →
`ascension`), et une liste de mots vides **relevée mot à mot** sur un vrai ES
(`tests/compat/releve_mots_vides.py`) — elle n'est ni celle de Snowball (qui
garde `est`) ni l'ancienne de Lucene (elle retire `ceci`, `cette`, `avec`,
`sans`, `ils`), donc la deviner n'était pas une option.

Les autres langues (`german`, `spanish`, `snowball`…) restent refusées : leur
stemmer n'est pas porté, et livrer sous le nom d'ES un analyzer qui indexe
autre chose changerait silencieusement les résultats d'un mapping existant.

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
| Tokenizers | 🟡 | Supporté : `standard`, `whitespace`, `keyword`, `letter`, `lowercase`. Refusé : `analysis.tokenizer` (un tokenizer défini par l'index — n-grams, `pattern`…) |
| Filtres | 🟡 | Supporté : `lowercase`, `asciifolding`, `stop` (liste explicite ou `_english_`). Refusé : tout filtre à base de stemmer (même raison que les analyzers de langue) |
| `char_filter` | ❌ | **pas encore** — aucun mapping venu d'une instance réelle n'en a encore demandé ; c'est une brique à écrire, pas un obstacle |
| Un analyzer de type autre que `custom` (`french`, `standard` paramétré…) | ❌ | **pas encore** — paramétrer un analyzer intégré (`stopwords`, `stem_exclusion`) demande de reproduire sa composition interne exacte, qui n'est mesurée que dans sa forme par défaut |

Le nom déclaré est celui que rend `_mapping`, et un analyzer sur mesure n'existe
que dans son index — `_analyze` sans index ne connaît que les intégrés.

**À savoir sur l'élision.** `standard` garde `l'édition` en **un seul terme**,
des deux côtés : c'est le filtre `elision` de l'analyzer `french` qui le
couperait, et il n'est pas encore là. Chercher `edition` ne trouve donc pas
`l'édition` — chez ES non plus, avec le même analyzer.

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
| `analyzer` | 🟡 | sur un champ `text` — voir la section dédiée. Supporté : `standard`, `simple`, `whitespace`, `keyword`, `stop`, `english`, `french`. Refusé : `search_analyzer`, les analyzers des autres langues |
| Multi-fields (`fields`) | ✅ | un seul niveau, comme ES. `titre.keyword` s'interroge et se trie comme un champ à part entière |
| `ignore_above` | ✅ | sur un `keyword` : au-delà, la valeur reste dans `_source` sans être indexée |
| Autres paramètres de champ (`index`, `null_value`, `doc_values`…) | ❌ | **pas encore** — les paramètres acceptés sont `type`, `analyzer`, `fields`, `ignore_above` et `format` ; les autres sont refusés plutôt qu'acceptés sans effet, faute de quoi un `index: false` laisserait croire qu'un champ n'est pas interrogeable |
| Noms de champ pointés (`a.b`) ou préfixés `_` | ❌ | **divergence de moteur** — un point est le séparateur de chemin d'un objet et un `_` initial est réservé aux métadonnées : accepter ces noms rendrait ambigu ce qu'un `client.ville` désigne |

## Ingestion

| Route | État | Détail |
|---|---|---|
| `PUT\|POST /{index}/_doc/{id}` | 🟡 | `_version`, `result`, `_seq_no`, `_primary_term`, `_shards`. `op_type=create` honoré. Refusé : `require_alias` (n'écrire que si la cible est un alias), `forced_refresh` (le champ de la réponse ; ES le rend sous `refresh=wait_for`) |
| `POST /{index}/_doc` | ✅ | identifiant généré par le serveur |
| `PUT\|POST /{index}/_create/{id}` | ✅ | 409 `version_conflict_engine_exception` si présent |
| `GET /{index}/_doc/{id}` | ✅ | temps réel : une écriture non rafraîchie est visible. `_source_includes` / `_source_excludes` / `_source` supportés |
| `HEAD /{index}/_doc/{id}` | ✅ | |
| `DELETE /{index}/_doc/{id}` | ✅ | 404 + `result: not_found` si absent, `_version` reste monotone |
| `POST\|PUT /_bulk`, `/{index}/_bulk` | 🟡 | NDJSON, actions `index` / `create` / `delete` / `update`, statut et erreur **par item**. Supporté : `_index` (métadonnée d'action), `_id` (métadonnée d'action). Refusé : les autres métadonnées (`_routing`, `if_seq_no`, `pipeline`…), `require_alias` |
| `refresh` (`true` / `false` / `wait_for`) | ✅ | `wait_for` est traité comme `true` : le commit est synchrone et mono-shard |
| `POST /{index}/_update/{id}` | 🟡 | Supporté : `doc` (fusion partielle), `upsert`, `doc_as_upsert`, `detect_noop`. Refusé : `script` (voir le scripting, hors périmètre), `_source` (filtrer la réponse d'un `_update`), `require_alias` |
| `GET\|POST /_mget`, `/{index}/_mget` | ✅ | formes `ids` et `docs`, filtrage de `_source`, erreur par document |
| `GET\|POST /{index}/_count` | ✅ | avec ou sans `query` |
| Versionnage optimiste `if_seq_no` / `if_primary_term` | ✅ | 409 `version_conflict_engine_exception` si le document a bougé |
| `version` / `version_type` externes | ❌ | **pas encore** — la version d'un document est gérée par ferrite ; l'imposer de l'extérieur demande de tenir un ordre que le serveur ne contrôle plus, et rien ne l'a encore réclamé |
| `_update_by_query`, `_delete_by_query`, `_reindex`, pipelines d'ingestion | ❌ | **pas encore** — ce sont des tâches longues, donc l'API `_tasks` et son suivi ; le même travail s'écrit aujourd'hui côté client avec `scroll` + `_bulk` |

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

`tests/compat/diff_multi_index.py` mesure tout ça contre un vrai ES 8.15 :
**87/87 appels identiques**, 0 divergence assumée, 0 écart. Le même fichier se
lance contre **deux** Elasticsearch (`--calibrer`) pour vérifier que ses verdicts
veulent dire quelque chose : 87/87.

## Alias

| Route | État | Détail |
|---|---|---|
| `POST /_aliases` | 🟡 | `index`/`indices` et `alias`/`aliases` au singulier comme au pluriel, motifs compris. Tout ou rien, comme chez ES — c'est ce qui rend une bascule atomique. Supporté : `add`, `remove`, `remove_index`. Refusé : `filter`, `routing` (`index_routing`, `search_routing`) |
| `PUT\|POST /{index}/_alias/{nom}` | ✅ | `{index}` est une expression, `{nom}` accepte une liste |
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
| `match_phrase` | 🟡 | les termes dans l'ordre, adjacents. Supporté : `query`, `boost`. Refusé : `slop` (voir les divergences), `analyzer` |
| `match_phrase_prefix` | 🟡 | les termes dans l'ordre, le dernier n'étant qu'un début de mot. Sur un champ `keyword`, refusée avec le message d'ES (« Can only use phrase prefix queries on text fields »). Supporté : `query`, `max_expansions` (défaut 50, comme ES), `boost`. Refusé : `slop`, `analyzer`, `zero_terms_query` |
| `exists` | 🟡 | sur tous les types, y compris `text`. Un champ absent, `null`, ou un tableau vide compte comme absent, comme chez ES. Refusé : sur un `text` dont la valeur ne produit **aucun terme** (une chaîne vide, des espaces, de la ponctuation seule : ES tient un `_field_names` et compte le champ présent dès qu'il est dans `_source` ; ferrite lit l'index inversé, où ces valeurs n'ont rien laissé, et rend donc **moins** de documents. Le corriger demanderait de stocker les valeurs de chaque champ `text` une seconde fois, en colonne — trouvé par [`fuzz_vs_es.py`](../tests/compat/fuzz_vs_es.py)) |
| `term` | ✅ | forme courte et forme `{value, boost}`. Sur un champ `date`, la valeur désigne la **période** qu'elle couvre, pas un instant, et le date math y est accepté (comme chez ES). `case_insensitive` ❌ |
| `ids` | ✅ | `values`, `boost` |
| `prefix` | 🟡 | non analysée comme chez ES. Supporté : `value`, `case_insensitive` (repliement ASCII, comme ES), `boost`. Refusé : `rewrite` |
| `wildcard` | 🟡 | `*`, `?`, et `\` qui échappe le caractère suivant. Supporté : `value`, `case_insensitive`, `boost`. Refusé : `rewrite` |
| `regexp` | 🟡 | syntaxe **Lucene**, ancrée des deux côtés (voir les divergences). Supporté : `value`, `flags`, `case_insensitive`, `boost`. Refusé : les opérateurs `~`, `&`, `<n-m>`, `#` (refusés explicitement, jamais pris pour des littéraux), `rewrite`, `max_determinized_states` |
| `fuzzy` | 🟡 | Supporté : `value`, `fuzziness` (`AUTO` ou distance entière), `transpositions`, `boost`. Refusé : `prefix_length`, `max_expansions`, `rewrite` |
| `constant_score` | ✅ | `filter`, `boost` |
| `dis_max` | ✅ | `queries`, `tie_breaker`, `boost` — voir [`src/dismax.rs`](../src/dismax.rs) |
| `terms` | 🟡 | liste de valeurs, score constant comme chez ES. Sur un champ `date`, chaque valeur est une période, comme dans `term`. Refusé : les *terms lookup* (lire la liste des valeurs dans un autre document) |
| `range` | 🟡 | sur `keyword` / numérique / `date` / `boolean`. Sur un champ `date`, les bornes acceptent le **date math** (`now`, `now-1d/d`, `2026-03-15\|\|+1M`) et sont **arrondies selon leur côté** — voir [la section dédiée](#date-math-et-arrondi-des-bornes). Supporté : `gte`, `gt`, `lte`, `lt`, `boost`, `format` (lecture des bornes). Refusé : `time_zone`, `relation`, un `range` sur un champ `text` |
| `bool` | 🟡 | `filter` ne contribue pas au score. Un `bool` qui n'a que des `must_not` matche tous les autres documents, comme chez ES. Supporté : `must`, `should`, `filter`, `must_not`, `boost`, `minimum_should_match` (ses **quatre notations**, voir [la section dédiée](#minimum_should_match)). Refusé : `_name`, `adjust_pure_negative` |
| `_name` (nommer une clause) | ❌ | **pas encore** — nommer une clause n'a d'interet qu'avec `matched_queries` dans la reponse, qui n'est pas rendu : accepter le nom en le perdant serait promettre une information qui ne reviendra pas. Refuse dans toutes les clauses, `bool` compris |
| `query_string`, `simple_query_string`, `function_score`, `boosting`, `intervals`, `terms_set`, `script`… | ❌ | **pas encore** — `parsing_exception: unknown query [...]`, avec la liste des clauses connues — la plus regrettée est `query_string`, dont la syntaxe est un langage à part entière |

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

### Corps et paramètres de `_search`

| | État | Détail |
|---|---|---|
| `POST\|GET /{index}/_search`, `POST\|GET /_search` | ✅ | `{index}` est une **expression** au sens d'ES (voir [Expressions d'index](#expressions-dindex-listes-motifs-alias)) ; sans index, la recherche porte sur tout, comme `_all`. Une recherche qui ne vise **aucun** index (cluster vide, motif sans correspondance) valide quand même son corps : requête, agrégations et tri sont lus contre un schéma vide avant qu'on conclue qu'il n'y a rien à chercher |
| `query` | ✅ | |
| `from` / `size` | ✅ | corps ou query string. `from + size > 10000` ❌ (`max_result_window`) |
| `sort` | 🟡 | multi-clés, `asc` / `desc`, sur `keyword` / numérique / `date` / `boolean`, plus `_score` et `_doc`. Valeurs manquantes en dernier (`missing: _last`). Le tableau `sort` est rendu dans chaque hit. En multi-index, un champ non mappé par un des index donne un échec **de ce shard**, comme chez ES. Refusé : `missing`, `mode`, `nested`, `unmapped_type`, le tri par script, le tri sur un champ `text` |
| `_source` | ✅ | `true` / `false`, chaîne, liste, `{includes, excludes}`, motifs `*`. Aussi via `_source_includes` / `_source_excludes` en query string |
| `fields` | 🟡 | la façon que la 7.10+ met en avant — et celle qu'envoie Kibana — de demander autre chose que le `_source` complet. Les valeurs sont lues dans le **`_source`** puis typées selon le mapping : l'ordre du document et ses **doublons** sont donc conservés (`["zoulou","alpha","alpha"]` ressort tel quel), et `{"tag": 42}` sur un `keyword` ressort `["42"]`. **La forme est ce qui compte** : chaque valeur est un tableau, même pour un champ mono-valué, et un champ absent n'a **pas de clé** — ce n'est pas une valeur nulle. Un multi-field (`titre.keyword`) est adressable, un sous-champ de `nested` se rend **groupé par élément** sous sa racine (`{"lignes": [{"ref": ["X1"]}, {"q": [5]}]}`, un élément sans valeur demandée étant omis), et un motif `*` ne ramène **pas** les métadonnées. Mesuré champ par champ par [`sonde_fields.py`](../tests/compat/sonde_fields.py). Supporté : `field` (un nom, un motif `*`, un multi-field, un chemin pointé), `format` (sur un champ `date` ; il remplace celui du mapping), `include_unmapped` (lit dans `_source` les chemins qu'aucun champ ne mappe — ce que Kibana envoie sur chaque recherche), `_id`, `_index` et `_version` nommés explicitement. Refusé : `_seq_no` et `_source` nommés dans `fields` (ES rend un **500** dessus (« Cannot fetch values for internal field ») ; un 500 ne se reproduit pas, ferrite les refuse explicitement), `?fields=` en query string (ES ne le connaît pas non plus — il le refuse comme un paramètre inconnu), un `format` qui n'est pas dans le vocabulaire de [`dateformat`](../src/dateformat.rs) (ES accepte un motif inconnu et rend une chaîne absurde (`format: "nawak"` rend `"0AM11AM24"`) ; ferrite le refuse, comme partout ailleurs où il lit un `format`) |
| `docvalue_fields` | 🟡 | la valeur telle qu'elle est **stockée en colonne**, et ce n'est pas la même que celle du `_source` : les colonnes sont triées, donc un `keyword` en ressort trié **et dédoublonné** (`["alpha","zoulou"]`) là où `fields` garde `["zoulou","alpha","alpha"]`, un numérique trié **avec** ses doublons (`[1,1,3]`), et un `float` avec la précision de son stockage sur 32 bits — ES rend `0.10000000149011612` là où le `_source` porte `0.1`. Accepté aussi en query string (`?docvalue_fields=`). Supporté : `field` (un nom, un motif, un multi-field, un chemin pointé), `format` (sur un champ `date`). Refusé : un champ `text` (il n'a pas de colonne ; ES fait échouer le shard (« Fielddata is disabled on [x] »), que le champ soit nommé ou attrapé par un motif — ferrite rend la même phrase), un `format` sur un champ numérique (ES l'interprète comme un `DecimalFormat` de Java (`format: "yyyy"` sur la valeur 1 rend `"yyyy1"`) ; ferrite ne l'imite pas), une métadonnée (`_id`, ...) (ES la refuse aussi (« Fielddata access on the _id field is disallowed »)) |
| `stored_fields` | 🟡 | **accepté, et il ne rend aucun champ — comme ES.** ferrite refuse `store` au mapping : aucun champ n'est stocké individuellement, et un Elasticsearch dont le mapping ne porte pas `store: true` ne rend rien non plus (mesuré). Reconstituer les valeurs depuis `_source` aurait rendu des valeurs qu'ES **ne rend pas** — c'est la raison pour laquelle ce n'est pas fait. Ce qui s'implémente, c'est ce que `stored_fields` change vraiment à la réponse, et qui se voit : il **retire `_source`** (sauf `_source` explicite), `_none_` retire **aussi `_id`**, et `_none_` avec `fields` est un 400. Accepté aussi en query string. Supporté : une liste de noms, un motif, `_none_`, `?stored_fields=` en query string. Refusé : `store` (le paramètre de mapping qui rendrait un champ stockable, refusé côté mapping — c'est lui qui rend ce refus-ci sans conséquence) |
| `script_fields`, `runtime_mappings` | ❌ | **hors périmètre assumé** — les deux définissent des champs **calculés par un script Painless**, que ferrite n'exécute pas. La mesure le confirme plutôt que la supposition : sur les 444 requêtes du corpus qui portent `runtime_mappings`, **425 l'envoient vide** (des gabarits de tracks Rally), et sur les 19 non vides **18 portent un script**. L'objet **vide** est donc accepté — il ne définit aucun champ, donc ne demande rien, et ES rend la même réponse avec ou sans (mesuré) ; un objet non vide est refusé explicitement |
| `track_total_hits` | 🟡 | le total est **toujours exact** (`relation: "eq"`). Supporté : `true`, une valeur numérique. Refusé : `false` (il n'y a rien à économiser sur un total déjà exact) |
| Scoring | 🟡 | BM25 (tantivy), `_score` et `max_score` renseignés ; `null` quand un tri est demandé, comme chez ES. Les **valeurs** ne sont pas comparées à celles d'ES (les constantes diffèrent) ; c'est l'**ordre** qui l'est, par [`diff_relevance.py`](../tests/compat/diff_relevance.py). Un `term` sur un champ numérique vaut `1.0` comme chez ES (requête de points), un `keyword` et un `boolean` sont indexés sans *fieldnorm* comme chez Lucene — donc deux documents qui portent la même valeur marquent pareil, quel que soit le nombre de valeurs du champ. Refusé : l'`avgdl` de BM25 sur un champ `text` **facultatif** (Lucene calcule la longueur moyenne sur les documents **qui ont le champ**, tantivy sur **tous** les documents de l'index. Deux scores voisins peuvent alors s'inverser. Mesuré par [`fuzz_vs_es.py`](../tests/compat/fuzz_vs_es.py) ; l'ampleur est mesurée par `diff_relevance.py`), le score d'un `fuzzy` (tantivy le rend **constant** ; Lucene pondère chaque terme par sa distance d'édition. Les documents rendus sont les mêmes, leur ordre non) |
| Format de réponse | ✅ | `took`, `timed_out`, `_shards` (avec `failures[]` quand un index n'a pas su répondre), `hits.total.{value,relation}`, `hits.max_score`, `hits.hits[]` avec `_index` / `_id` / `_score` / `_source` / `sort` |
| `preference` | 🟡 | accepté, sans objet : il n'y a qu'un shard |
| `aggs` / `aggregations` | 🟡 | voir la section dédiée |
| `scroll` | ✅ | `?scroll=1m` ouvre un contexte figé et rend un `_scroll_id` — voir la section dédiée |
| `highlight`, `search_after`, `pit`, `collapse`, `knn`, `explain`, `seq_no_primary_term`, `post_filter`, `min_score`, `suggest`, `rescore`, `track_scores`, `q`, `timeout`, `terminate_after`, `version`, `indices_boost`, `profile`, `slice`, `stats`, `ext`, `retriever` | ❌ | **pas encore** — aucun n'est un obstacle de moteur ; `highlight` et `search_after` sont les deux qui manquent le plus, le premier pour une liste de résultats, le second pour paginer au-delà de 10 000 |
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
| `time_zone` | ❌ | **pas encore** — il déplace les arrondis, donc les résultats ; l'accepter sans l'appliquer rendrait les mauvais documents en silence |

Une expression malformée est refusée avec **le message d'ES, mot pour mot**
(`unit [q] not supported for date math [-1q]`, `truncated date math [/]`,
`operator not supported for date math [1d]`, `For input string: "…"`). ES les
rend sous un `search_phase_execution_exception` « all shards failed » dont la
`root_cause` porte ce texte ; ferrite rend l'erreur directement, sans cet
empilement.

## Agrégations

Comparées champ par champ à un vrai ES 8.15 sur 53 requêtes
(`tests/compat/diff_aggs.py`), clés de réponse comprises.

| Agrégation | État | Détail |
|---|---|---|
| `min`, `max`, `sum`, `avg`, `value_count`, `stats` | ✅ | `field`, `missing`. Sur un champ `date`, la valeur est en millisecondes et le `*_as_string` est rendu comme chez ES |
| `terms` | 🟡 | `sum_other_doc_count` est renseigné, et `doc_count_error_upper_bound` suit la règle d'ES : `-1` quand l'ordre est `_count` **croissant** et que le nombre de termes distincts atteint `shard_size` (`size × 1,5 + 10` par défaut), `0` partout ailleurs. Sur un champ `date`, la clé du bucket est rendue en millisecondes avec son `key_as_string`, comme chez ES. Supporté : `field`, `size`, `shard_size`, `min_doc_count` (sa valeur par défaut (`1`) seulement — voir ci-dessous), `order` (`_count` / `_key` seulement). Refusé : `min_doc_count` autre que sa valeur par défaut (`1`) (à `0`, il demande un bucket pour les valeurs que la recherche n'a **pas** trouvées, et l'agrégation de tantivy ne le rend pas de façon fiable : zéro bucket sur une colonne numérique, zéro bucket quand la requête ne ramène rien, et des buckets vides privés de leurs sous-agrégations. Au-delà de `1`, c'est `sum_other_doc_count` qui ne suit plus : la règle d'ES a été cherchée pour de bon, une formule ajustée sur quinze formes d'un corpus les collait toutes puis s'est effondrée sur d'autres (27 écarts sur 1 450 cas tirés au sort). Elle dépend de l'ordre demandé, de la troncature et de l'ordre de parcours du dictionnaire de termes — c'est le collecteur d'ES qu'il faudrait réécrire, et annoncer un compte faux serait pire. Mesuré par [`fuzz_vs_es.py`](../tests/compat/fuzz_vs_es.py)), `include`, `exclude`, `missing`, `collect_mode`, `execution_hint`, `script`, `shard_min_doc_count`, `show_term_doc_count_error`, l'ordre par sous-agrégation |
| `range` | 🟡 | `ranges` avec `from` / `to` / `key`, `keyed`. Sur un champ `date`, les bornes s'écrivent **en dates** (au `format` du champ) et les buckets rendent `from_as_string` / `to_as_string`. Les intervalles que le client n'a pas demandés sont écartés : tantivy comble les trous entre deux bornes, Elasticsearch non. Refusé : un **trou** entre deux intervalles, sur un champ `date` (tantivy comble les trous et ferrite écarte ensuite le bucket de remplissage ; sur une date, où les bornes passent en nanosecondes, ce remplissage avale l'intervalle demandé. Sur un champ numérique, les deux buckets sortent et le filtrage suffit), des intervalles qui se **chevauchent** (ES compte alors un document dans chaque bucket qui le contient ; l'agrégation de tantivy partitionne les valeurs et ne sait pas le faire), un champ **multivalué** (voir la ligne suivante) |
| `histogram` | 🟡 | `interval`, `offset`, `min_doc_count`, `hard_bounds`, `extended_bounds`, `keyed`. Refusé : un champ **multivalué** (voir la ligne suivante) |
| `date_histogram` | 🟡 | Supporté : `field`, `fixed_interval`, `offset`, `min_doc_count`, `hard_bounds`, `extended_bounds`, `keyed` (comme `histogram`). Refusé : `calendar_interval` (mois et années civils n'ont pas d'équivalent dans tantivy), `time_zone`, `format`, `order` |
| Sous-agrégations | ✅ | sur tous les types de buckets, vérifiées jusqu'à trois niveaux. Un bucket **vide** porte les siennes, comme chez ES : tantivy comble les trous d'un `histogram` sans exécuter ce qu'il y a dessous, et ferrite y remet la forme « zéro document » — mesurée sur une recherche qui ne ramène rien, pas écrite à la main |
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

## Limites connues (perf, pas fonctionnalité)

- **Le tri charge tous les hits en mémoire.** Le collecteur de tri ramasse tous
  les documents correspondants avec leurs clés avant de les ordonner. C'est
  correct pour toutes les combinaisons de clés (y compris `keyword` et
  multi-clés, où un tri par ordinal de terme serait faux entre segments), mais
  l'occupation mémoire est proportionnelle au nombre de résultats. À revoir
  quand le tri deviendra un chemin chaud. La recherche **sans** tri utilise un
  top-K classique et n'a pas cette limite.
- **Un contexte de `scroll` tient toute la liste des correspondances en
  mémoire** (une adresse et ses clés de tri par document), plus l'instantané de
  l'index. C'est le prix de « chaque document une fois, et une seule, en un seul
  balayage » ; l'alternative — rejouer la requête à chaque page — coûterait N
  recherches pour N pages et ne figerait rien. Les contextes expirés sont purgés
  toutes les 30 s, et 500 au plus peuvent être ouverts.
- **`GET /{index}/_doc/{id}` déclenche un commit** si des écritures sont en
  attente, pour rester temps réel comme ES. Sous forte charge d'écriture, un
  `get` peut donc coûter cher.
- **La table `_id → (_version, _seq_no)` est en mémoire** et reconstruite au
  démarrage en relisant les fast fields de l'index. Coût proportionnel au
  nombre de documents au démarrage.
