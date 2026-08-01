# compat — ce que ferrite sait faire, et ce qu'il refuse

Inventaire du support de l'API Elasticsearch par ferrite. **Mis à jour dans la
PR qui change le comportement, pas après.**

| | |
|---|---|
| ✅ | supporté, vérifié par le harnais de compat (`tests/compat/`) |
| 🟡 | partiel — la partie supportée est décrite, le reste est refusé |
| ❌ | refusé **explicitement**, avec une erreur au format d'Elasticsearch |

La règle qui prime : **jamais d'échec silencieux**. Rien de ce qui figure en ❌
ne renvoie « 0 résultat » ou un résultat partiel — tout produit une erreur
lisible. Le type d'erreur `not_implemented_in_ferrite_exception` signale
précisément « Elasticsearch sait faire, ferrite pas encore ».

Version d'API annoncée : **Elasticsearch 8.15.0** (`version.number`,
`_nodes`). Toutes les réponses portent `X-elastic-product: Elasticsearch`.

---

## Poignée de main et cluster

| Route | État | Détail |
|---|---|---|
| `GET /` | ✅ | `name`, `cluster_name`, `cluster_uuid`, bloc `version` complet, `tagline` |
| `HEAD /` (`ping`) | ✅ | |
| `GET /_cluster/health`, `/_cluster/health/{index}` | 🟡 | toujours `green`, 1 nœud, 0 shard non assigné. `wait_for_status` et `timeout` sont acceptés et sans objet (déjà vert) ; `level` est ❌ (il change la forme de la réponse) |
| `GET /_cat/indices`, `/_cat/indices/{index}` | 🟡 | texte aligné par défaut, `?format=json`, `?v`. `h`, `s`, `bytes` sont ❌ |
| `GET /_cat/health` | 🟡 | idem |
| `GET /_nodes`, `/_nodes/{spec}` | 🟡 | un nœud, champs d'identité et `http` ; pas de `settings`, `os`, `jvm`. `{spec}` accepte `_all`, `_local`, `_master` et l'identifiant du nœud ; les sous-ressources (`_nodes/stats`, `_nodes/hot_threads`…) sont ❌ |
| Tout le reste de `_cluster/*`, `_cat/*`, `_nodes/*` | ❌ | `no handler found for uri [...]` |

## Index et mapping

| | État | Détail |
|---|---|---|
| `PUT /{index}` | 🟡 | `mappings` est **optionnel** (les champs viendront des documents). `settings` limité à `number_of_shards` / `number_of_replicas` (acceptés, sans effet : ferrite est mono-shard). `aliases` doit être vide |
| `DELETE /{index}` | ✅ | `ignore_unavailable` honoré |
| `HEAD /{index}` | ✅ | |
| `GET /{index}` | ✅ | `aliases` / `mappings` / `settings` |
| `GET /{index}/_mapping` | ✅ | |
| `PUT /{index}/_mapping` | 🟡 | **ajoute** des champs (une nouvelle génération est construite). Changer le type d'un champ existant reste refusé, comme chez ES. Modifier `dynamic` : ❌ |
| `POST /{index}/_refresh` | ✅ | |
| `POST\|GET /_analyze`, `/{index}/_analyze` | 🟡 | `text` (chaîne ou liste), `analyzer`, `field`. `tokenizer` / `filter` / `char_filter` explicites : ❌ |
| Mapping dynamique | ✅ | `dynamic` : `true` (défaut), `false`, `strict`. `runtime` ❌. Voir plus bas |
| Alias, templates, ILM, `_settings`, `_stats`, `_close`, `_open` | ❌ | |

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
28 textes français et anglais (`tests/compat/diff_analyzers.py`).

| Analyzer | État |
|---|---|
| `standard` (défaut) | ✅ identique à ES sur les 28 textes |
| `simple` | ✅ identique |
| `whitespace` | ✅ identique |
| `keyword` | ✅ identique |
| `stop` | ✅ identique |
| `french`, `english`, `snowball` et les autres analyzers de langue | ❌ **refus assumé** |
| Analyzers sur mesure (`settings.analysis`) | ❌ |

**Pourquoi les analyzers de langue sont refusés.** Ils reposent sur un stemmer,
et celui de tantivy (Snowball) n'est pas celui de Lucene (stemmer *léger* pour
le français, Porter pour l'anglais). Mesuré sur les mêmes 28 textes : **17
donnent des termes différents en `french`, 19 en `english`**. Par exemple
« Horla » devient `horl` chez tantivy et `horla` chez ES, « mineurs » `mineur`
contre `mineu`, « arriviste » `arriv` contre `arivist`.

Porter le nom d'ES en indexant autre chose changerait silencieusement le
comportement d'un mapping existant — précisément ce que ce projet refuse. Les
supporter demande de porter les stemmers de Lucene, ce qui mérite sa propre
itération.

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
| `float`, `double` | ✅ | `f64` indexé + fast |
| `boolean` | ✅ | `bool` indexé + fast |
| `date` | 🟡 | `date` (millisecondes) indexé + fast. Formats acceptés : `strict_date_optional_time` (ISO-8601, avec ou sans fuseau, date seule) et `epoch_millis`. `format` personnalisé : ❌ |
| Tableaux de valeurs | ✅ | tout champ accepte une valeur ou un tableau |
| `null` | ✅ | ignoré à l'indexation, comme chez ES (pas de `null_value`) |
| Tout autre type (`geo_point`, `nested`, `object`, `ip`, `binary`…) | ❌ | |
| Sous-objet dans un **document** (`{"a": {"b": 1}}`) | ❌ | refus explicite à l'indexation, **y compris dans un tableau** (`{"a": [{"b": 1}]}`) : sans ce refus le champ serait gardé dans `_source` mais absent du mapping, donc introuvable |
| `analyzer` | 🟡 | sur un champ `text` : `standard` (défaut), `simple`, `whitespace`, `keyword`, `stop` — voir la section dédiée |
| Multi-fields (`fields`) | ✅ | un seul niveau, comme ES. `titre.keyword` s'interroge et se trie comme un champ à part entière |
| `ignore_above` | ✅ | sur un `keyword` : au-delà, la valeur reste dans `_source` sans être indexée |
| Autres paramètres de champ (`index`, `format`, `null_value`, `doc_values`…) | ❌ | acceptés : `type`, `analyzer`, `fields`, `ignore_above` |
| Noms de champ pointés (`a.b`) ou préfixés `_` | ❌ | |

## Ingestion

| Route | État | Détail |
|---|---|---|
| `PUT\|POST /{index}/_doc/{id}` | ✅ | `_version`, `result`, `_seq_no`, `_primary_term`, `_shards`. `op_type=create` honoré |
| `POST /{index}/_doc` | ✅ | identifiant généré par le serveur |
| `PUT\|POST /{index}/_create/{id}` | ✅ | 409 `version_conflict_engine_exception` si présent |
| `GET /{index}/_doc/{id}` | ✅ | temps réel : une écriture non rafraîchie est visible. `_source_includes` / `_source_excludes` / `_source` supportés |
| `HEAD /{index}/_doc/{id}` | ✅ | |
| `DELETE /{index}/_doc/{id}` | ✅ | 404 + `result: not_found` si absent, `_version` reste monotone |
| `POST\|PUT /_bulk`, `/{index}/_bulk` | 🟡 | NDJSON, actions `index` / `create` / `delete` / `update`, statut et erreur **par item**. Métadonnées acceptées : `_index`, `_id` ; les autres (`_routing`, `if_seq_no`, `pipeline`…) sont ❌ |
| `refresh` (`true` / `false` / `wait_for`) | ✅ | `wait_for` est traité comme `true` : le commit est synchrone et mono-shard |
| `POST /{index}/_update/{id}` | 🟡 | `doc` (fusion partielle), `upsert`, `doc_as_upsert`, `detect_noop`. Les scripts : ❌ |
| `GET\|POST /_mget`, `/{index}/_mget` | ✅ | formes `ids` et `docs`, filtrage de `_source`, erreur par document |
| `GET\|POST /{index}/_count` | ✅ | avec ou sans `query` |
| Versionnage optimiste `if_seq_no` / `if_primary_term` | ✅ | 409 `version_conflict_engine_exception` si le document a bougé |
| `version` / `version_type` externes | ❌ | |
| `_update_by_query`, `_delete_by_query`, `_reindex`, pipelines d'ingestion | ❌ | |

Sans `refresh`, une écriture devient visible **au plus tard après 1 seconde**
(équivalent du `index.refresh_interval` d'ES). Avec `refresh`, la visibilité est
garantie au retour de l'appel, y compris si le rafraîchissement de fond est en
train de tourner — les rafraîchissements sont sérialisés entre eux.

## Recherche

`POST\|GET /{index}/_search` ✅. `POST\|GET /_search` (multi-index) ❌ — il faut
nommer un index ; les motifs et les listes (`livres*`, `a,b`) sont ❌ eux aussi,
avec une erreur qui dit pourquoi.

### Clauses du Query DSL

| Clause | État | Détail |
|---|---|---|
| `match_all` | ✅ | `boost` |
| `match_none` | ✅ | |
| `match` | 🟡 | `query`, `operator` (`or` / `and`), `boost`. Sur un champ non analysé, se comporte comme `term`. `fuzziness`, `minimum_should_match`, `analyzer`, `zero_terms_query`, `prefix_length` : ❌ |
| `multi_match` | 🟡 | `query`, `fields` (**obligatoire**, avec la pondération `champ^3`), `type` `best_fields` (défaut) et `most_fields`, `operator`, `tie_breaker`, `boost`. `cross_fields`, `phrase`, `phrase_prefix`, `bool_prefix` et les motifs de champ (`tit*`) : ❌ |
| `match_phrase` | 🟡 | les termes dans l'ordre, adjacents. `boost`. `slop` : ❌ (voir les divergences) |
| `exists` | ✅ | sur tous les types, y compris `text`. Un champ absent, `null`, ou un tableau vide compte comme absent, comme chez ES |
| `term` | ✅ | forme courte et forme `{value, boost}`. `case_insensitive` ❌ |
| `ids` | ✅ | `values`, `boost` |
| `prefix` | 🟡 | non analysée comme chez ES. `case_insensitive` / `rewrite` : ❌ |
| `wildcard` | 🟡 | `*` et `?`. `case_insensitive` / `rewrite` : ❌ |
| `fuzzy` | 🟡 | `fuzziness` (`AUTO` ou distance entière), `transpositions`, `boost`. `prefix_length` / `max_expansions` / `rewrite` : ❌ |
| `constant_score` | ✅ | `filter`, `boost` |
| `dis_max` | ✅ | `queries`, `tie_breaker`, `boost` — voir [`src/dismax.rs`](../src/dismax.rs) |
| `terms` | 🟡 | liste de valeurs, score constant comme chez ES. Les *terms lookup* sont ❌ |
| `range` | 🟡 | `gte`, `gt`, `lte`, `lt`, `boost`, sur `keyword` / numérique / `date` / `boolean`. Sur un champ `text` : ❌. `format`, `time_zone`, `relation` : ❌ |
| `bool` | 🟡 | `must`, `should`, `filter`, `must_not`, `boost`, et `minimum_should_match` **sous forme entière** (les pourcentages et expressions sont ❌). `filter` ne contribue pas au score. Un `bool` qui n'a que des `must_not` matche tous les autres documents, comme chez ES |
| `query_string`, `simple_query_string`, `regexp`, `nested`, `function_score`, `boosting`, `match_phrase_prefix`, `terms_set`, `script`… | ❌ | `parsing_exception: unknown query [...]` |

### Corps et paramètres de `_search`

| | État | Détail |
|---|---|---|
| `query` | ✅ | |
| `from` / `size` | ✅ | corps ou query string. `from + size > 10000` ❌ (`max_result_window`) |
| `sort` | 🟡 | multi-clés, `asc` / `desc`, sur `keyword` / numérique / `date` / `boolean`, plus `_score` et `_doc`. Valeurs manquantes en dernier (`missing: _last`). Le tableau `sort` est rendu dans chaque hit. Tri sur un champ `text` ❌ ; `missing`, `mode`, `nested`, tri par script ❌ |
| `_source` | ✅ | `true` / `false`, chaîne, liste, `{includes, excludes}`, motifs `*`. Aussi via `_source_includes` / `_source_excludes` en query string |
| `track_total_hits` | 🟡 | le total est **toujours exact** (`relation: "eq"`), donc `true` et une valeur numérique sont acceptés ; `false` est ❌ |
| Scoring | ✅ | BM25 (tantivy), `_score` et `max_score` renseignés ; `null` quand un tri est demandé, comme chez ES |
| Format de réponse | ✅ | `took`, `timed_out`, `_shards`, `hits.total.{value,relation}`, `hits.max_score`, `hits.hits[]` avec `_index` / `_id` / `_score` / `_source` / `sort` |
| `preference` | 🟡 | accepté, sans objet : il n'y a qu'un shard |
| `aggs` / `aggregations` | 🟡 | voir la section dédiée |
| `highlight`, `search_after`, `scroll`, PIT, `collapse`, `knn`, `explain`, `fields`, `post_filter`, `min_score`, `suggest`, `rescore`, `track_scores`, `q` | ❌ | |
| `ignore_unavailable`, `allow_no_indices`, `expand_wildcards`, `routing`, `filter_path`, `typed_keys` | ❌ | ils n'ont de sens qu'avec des motifs multi-index ou changent la forme de la réponse |
| `rest_total_hits_as_int` | ❌ | il change la forme de `hits.total` (nombre au lieu d'objet). Accepté par ES 8, refusé ici : du code venu de la 6.x/7.x s'en sert encore, voir [`compat-es7.md`](compat-es7.md) |
| `_msearch`, `_search/template`, `_explain`, `_validate` | ❌ | |

Les paramètres purement cosmétiques `pretty`, `human` et `error_trace` sont
acceptés partout ; `pretty` est implémenté (indentation de la réponse).

**Tout paramètre de query string non reconnu est refusé** avec
`request [...] contains unrecognized parameter: [...]`, comme chez ES.

## Agrégations

Comparées champ par champ à un vrai ES 8.15 sur 34 requêtes
(`tests/compat/diff_aggs.py`), clés de réponse comprises.

| Agrégation | État | Détail |
|---|---|---|
| `min`, `max`, `sum`, `avg`, `value_count`, `stats` | ✅ | `field`, `missing`. Sur un champ `date`, la valeur est en millisecondes et le `*_as_string` est rendu comme chez ES |
| `terms` | 🟡 | `field`, `size`, `shard_size`, `min_doc_count`, `order` (`_count` / `_key` seulement). `doc_count_error_upper_bound` et `sum_other_doc_count` sont renseignés. `include` / `exclude` / ordre par sous-agrégation : ❌ |
| `range` | ✅ | `ranges` avec `from` / `to` / `key`, `keyed` |
| `histogram` | ✅ | `interval`, `offset`, `min_doc_count`, `hard_bounds`, `extended_bounds`, `keyed` |
| `date_histogram` | 🟡 | `fixed_interval` et les mêmes paramètres. `calendar_interval` ❌ (mois et années civils n'ont pas d'équivalent dans tantivy) |
| Sous-agrégations | ✅ | sur tous les types de buckets, vérifiées jusqu'à trois niveaux |
| `cardinality` | ❌ | **refus assumé** : l'estimation de tantivy diffère de celle d'ES (mesuré : 582 valeurs distinctes annoncées là où ES en compte 598), y compris sous le seuil où ES est exact |
| `filter` | ❌ | l'agrégation `filter` de tantivy prend une chaîne dans sa propre syntaxe de requête, pas une requête du Query DSL : la traduction serait approximative |
| `percentiles`, `extended_stats`, `top_hits`, `composite`, `filters`, `nested`, `significant_terms`, `date_range`, `ip_range`… | ❌ | |

Agréger sur un champ `text` est refusé, comme chez ES (`Fielddata is disabled`) :
utiliser son multi-field `.keyword`.

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

1. **Un champ inconnu dans une requête est une erreur, pas 0 résultat.**
   ES renvoie `hits.total = 0` quand on interroge un champ absent du mapping.
   Sans mapping dynamique, ce cas est toujours un bug du client, et répondre
   « aucun résultat » serait exactement le résultat faux présenté comme complet
   que ce projet refuse. ferrite renvoie `query_shard_exception`.

2. **`slop` est refusé dans `match_phrase`.** tantivy et Lucene ne comptent pas
   les déplacements de la même façon dès que la phrase dépasse deux termes :
   cherchée comme `un deux trois`, la phrase `deux un trois` correspond à
   `slop: 2` chez Elasticsearch et seulement à `slop: 3` chez tantivy. Accepter
   le paramètre ferait donc rendre à ferrite **moins de documents** qu'ES sur la
   même requête, sans que rien ne le signale. La phrase exacte (`slop` absent ou
   `0`) est vérifiée identique à ES.

3. **`best_fields` n'utilise pas le `DisjunctionMaxQuery` de tantivy.**
   Dans tantivy 0.26 cette requête rend la **somme** des scores et non leur
   maximum, quel que soit le `tie_breaker` (le combineur est court-circuité par
   une spécialisation interne, et le constructeur correct est `pub(crate)`).
   S'en servir donnerait silencieusement un classement `most_fields` à qui
   demande `best_fields`. ferrite implémente donc `dis_max` lui-même dans
   `src/dismax.rs`, en déléguant le parcours des documents à tantivy et en ne
   recalculant que le score. Un test unitaire verrouille « max, pas somme » pour
   qu'une montée de version ne puisse pas dégrader la pertinence en silence.

4. **Analyse du texte.** Les champs `text` utilisent le tokenizer `default` de
   tantivy (découpe sur les non-alphanumériques + minuscules + rejet des tokens
   de plus de 40 caractères). Très proche de l'analyzer `standard` d'ES pour du
   texte latin, mais ce n'est pas la même implémentation : sur de l'unicode
   exotique ou du CJK, les tokens peuvent différer.

5. **Les scores ne sont pas identiques à ceux d'ES.** Même formule (BM25), mais
   statistiques d'index et normalisation de longueur différentes. L'*ordre* des
   résultats est comparé à celui d'ES par `tests/compat/diff_against_es.py` ;
   les valeurs absolues, non.

6. **`_shards.total` vaut 1** (un shard, zéro réplique) là où un ES par défaut
   annonce 2 dans les réponses d'écriture.

7. **`_cluster/health` est toujours `green`.** C'est le comportement voulu pour
   un mono-nœud : il n'y a pas de réplique à assigner.

8. **`wait_for` vaut `true` pour `refresh`.** Le commit est synchrone, il n'y a
   rien à attendre.

## Limites connues (perf, pas fonctionnalité)

- **Le tri charge tous les hits en mémoire.** Le collecteur de tri ramasse tous
  les documents correspondants avec leurs clés avant de les ordonner. C'est
  correct pour toutes les combinaisons de clés (y compris `keyword` et
  multi-clés, où un tri par ordinal de terme serait faux entre segments), mais
  l'occupation mémoire est proportionnelle au nombre de résultats. À revoir
  quand le tri deviendra un chemin chaud. La recherche **sans** tri utilise un
  top-K classique et n'a pas cette limite.
- **`GET /{index}/_doc/{id}` déclenche un commit** si des écritures sont en
  attente, pour rester temps réel comme ES. Sous forte charge d'écriture, un
  `get` peut donc coûter cher.
- **La table `_id → (_version, _seq_no)` est en mémoire** et reconstruite au
  démarrage en relisant les fast fields de l'index. Coût proportionnel au
  nombre de documents au démarrage.
