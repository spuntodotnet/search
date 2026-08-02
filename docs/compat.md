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

**La suite de conformance d'Elasticsearch elle-même** (`tests/compat/conformance_es.py`,
643 cas de la 7.10.2 — la dernière version Apache 2.0) donne l'état d'ensemble :

| | ferrite | ES 7.10.2 (validation du runner) |
|---|---|---|
| réussis | 65 | 537 |
| refusés explicitement (hors périmètre) | 324 | 0 |
| sautés (version, fonctionnalité du runner) | 98 | 103 |
| **échecs** | **156** | 3 |

Les 156 échecs sont l'inventaire des écarts qui restent — les plus gros sont
listés dans [`conformance.md`](conformance.md). C'est la mesure la moins
complaisante du projet : les cas viennent d'Elastic, pas de nous.

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
| Création à l'écriture | ✅ | indexer dans un index absent le **crée**, comme ES (`action.auto_create_index`) : `index`, `create`, `update`, et le `_bulk`. La lecture, la recherche et la suppression rendent toujours 404 |
| Expressions d'index (`a,b`, `logs-*`, `_all`, exclusions, alias) | ✅ | sur **toutes** les routes, recherche comprise — voir la section dédiée |
| `PUT /{index}` | 🟡 | `mappings` est **optionnel** (les champs viendront des documents). `settings` limité à `number_of_shards` / `number_of_replicas` (acceptés, sans effet : ferrite est mono-shard). `aliases` ✅ |
| `DELETE /{index}` | ✅ | listes et motifs, sous `action.destructive_requires_name` (voir plus bas). `ignore_unavailable` honoré |
| `HEAD /{index}` | ✅ | 200 dès que l'expression se résout, même sur zéro index — comme ES |
| `GET /{index}` | ✅ | `aliases` / `mappings` / `settings`, une entrée par index visé |
| `GET /{index}/_mapping` | ✅ | |
| `PUT /{index}/_mapping` | 🟡 | **ajoute** des champs (une nouvelle génération est construite). Changer le type d'un champ existant reste refusé, comme chez ES. Modifier `dynamic` : ❌ |
| `POST /{index}/_refresh` | ✅ | |
| `POST\|GET /_analyze`, `/{index}/_analyze` | 🟡 | `text` (chaîne ou liste), `analyzer`, `field`. `tokenizer` / `filter` / `char_filter` explicites : ❌ |
| Mapping dynamique | ✅ | `dynamic` : `true` (défaut), `false`, `strict`. `runtime` ❌. Voir plus bas |
| Alias | ✅ | voir la section dédiée |
| Templates, ILM, `_settings`, `_stats`, `_close`, `_open` | ❌ | |

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

| Analyzer | État |
|---|---|
| `standard` (défaut) | ✅ identique à ES sur les 28 textes |
| `simple` | ✅ identique |
| `whitespace` | ✅ identique |
| `keyword` | ✅ identique |
| `stop` | ✅ identique |
| `english` | ✅ identique — Porter porté depuis Lucene, filtre possessif compris |
| `french` | ✅ identique — stemmer léger de Savoy, élision, mots vides relevés |
| `german`, `spanish`, `snowball` et les autres langues | ❌ leur stemmer n'est pas porté |
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
| Tokenizers | 🟡 | `standard`, `whitespace`, `keyword`, `letter`, `lowercase`. Les tokenizers définis dans `analysis.tokenizer` (n-grams, `pattern`…) : ❌ |
| Filtres | 🟡 | `lowercase`, `asciifolding`, `stop` (liste explicite ou `_english_`). Tout filtre à base de stemmer : ❌, pour la même raison que les analyzers de langue |
| `char_filter` | ❌ | |
| Un analyzer de type autre que `custom` (`french`, `standard` paramétré…) | ❌ | |

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
| `float`, `double` | ✅ | `f64` indexé + fast |
| `boolean` | ✅ | `bool` indexé + fast |
| `date` | 🟡 | `date` (millisecondes) indexé + fast. **`format` supporté** : motifs Java (`yyyy`, `yy`, `MM`, `dd`, `HH`, `hh`, `mm`, `ss`, `SSS`, `a`, `Z`, texte entre apostrophes), alternatives `\|\|`, et les noms prédéfinis courants (`strict_date_optional_time`, `epoch_millis`, `epoch_second`, `date`, `date_time`, `basic_date`…). Le format sert à lire (indexation, bornes d'un `range`) **et** à rendre (`*_as_string`). Une lettre non traduite (`G`, `w`, `e`…) est refusée explicitement plutôt qu'ignorée |
| Tableaux de valeurs | ✅ | tout champ accepte une valeur ou un tableau |
| `null` | ✅ | ignoré à l'indexation, comme chez ES (pas de `null_value`) |
| `object` (sous-objet), déclaré ou deviné | ✅ | indexé par **chemins pointés** (`client.ville`), comme ES. Un objet n'est pas un champ : il n'existe que par ses feuilles. `GET /_mapping` re-niche les chemins. Un tableau d'objets est aplati — comme ES, la correspondance entre sous-champs d'un même élément est perdue (c'est ce que `nested` corrige) |
| `nested` | 🟡 | voir [la section dédiée](#nested) |
| `join` (parent/enfant) | 🟡 | voir [la section dédiée](#join-parentenfant) |
| Tout autre type (`geo_point`, `ip`, `binary`…) | ❌ | |
| `analyzer` | 🟡 | sur un champ `text` : `standard` (défaut), `simple`, `whitespace`, `keyword`, `stop` — voir la section dédiée |
| Multi-fields (`fields`) | ✅ | un seul niveau, comme ES. `titre.keyword` s'interroge et se trie comme un champ à part entière |
| `ignore_above` | ✅ | sur un `keyword` : au-delà, la valeur reste dans `_source` sans être indexée |
| Autres paramètres de champ (`index`, `null_value`, `doc_values`…) | ❌ | acceptés : `type`, `analyzer`, `fields`, `ignore_above`, `format` |
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

`POST|GET /{index}/_search` ✅, et `{index}` est une **expression** au sens
d'Elasticsearch — voir [Expressions d'index](#expressions-dindex-listes-motifs-alias)
juste en dessous. `POST|GET /_search` sans index cherche partout, comme `_all`.

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
| `expand_wildcards` | 🟡 | `open`, `hidden`, `all` sont équivalents (ferrite n'a ni index fermé ni index caché) ; `closed` seul ne désigne donc rien ; `none` est ❌ |

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
**86/87 appels identiques**, 1 divergence assumée, 0 écart. Le même fichier se
lance contre **deux** Elasticsearch (`--calibrer`) pour vérifier que ses verdicts
veulent dire quelque chose : 87/87.

## Alias

| Route | État | Détail |
|---|---|---|
| `POST /_aliases` | 🟡 | `add`, `remove`, `remove_index` ; `index`/`indices` et `alias`/`aliases` au singulier comme au pluriel, motifs compris. Tout ou rien, comme chez ES — c'est ce qui rend une bascule atomique |
| `PUT\|POST /{index}/_alias/{nom}` | ✅ | `{index}` est une expression, `{nom}` accepte une liste |
| `DELETE /{index}/_alias/{nom}` | ✅ | `{nom}` accepte un motif |
| `GET /_alias`, `/_alias/{nom}`, `/{index}/_alias`, `/{index}/_alias/{nom}` | ✅ | y compris le 404 « à corps de chaîne » d'ES (`{"error": "alias [x] missing", "status": 404}`) |
| `HEAD /_alias/{nom}`, `/{index}/_alias/{nom}` | ✅ | |
| `aliases` dans `PUT /{index}` | ✅ | posé après la création ; un alias refusé annule la création plutôt que de laisser une demande à moitié faite |
| `is_write_index` | ✅ | désigne l'index qui reçoit les écritures quand l'alias en couvre plusieurs |
| `filter`, `routing`, `index_routing`, `search_routing` sur un alias | ❌ | un alias filtré dont le filtre n'est pas appliqué rendrait précisément les documents qu'il est censé cacher ; le routage n'a rien à choisir sur un mono-shard |

Écrire à travers un alias qui couvre plusieurs index est refusé tant qu'aucun
`is_write_index` ne tranche — choisir à la place du client écrirait
silencieusement au mauvais endroit. La réponse porte alors le nom **concret** de
l'index, pas celui de l'alias, comme chez ES.

Un index et un alias ne peuvent pas porter le même nom (la résolution ne saurait
plus lequel désigner) ; supprimer un index le retire de ses alias ; et
`DELETE /{alias}` est refusé — effacer des index que le client n'a pas nommés
n'est pas une suppression, c'est un accident.

## Réglages de cluster

| | État | Détail |
|---|---|---|
| `GET\|PUT /_cluster/settings` | 🟡 | `persistent` et `transient` (le second l'emporte), écriture plate ou imbriquée. Seul `action.destructive_requires_name` est reconnu ; tout autre réglage est refusé avec le message d'ES (`not recognized`) |
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
| `sort` | 🟡 | multi-clés, `asc` / `desc`, sur `keyword` / numérique / `date` / `boolean`, plus `_score` et `_doc`. Valeurs manquantes en dernier (`missing: _last`). Le tableau `sort` est rendu dans chaque hit. En multi-index, un champ non mappé par un des index donne un échec **de ce shard**, comme chez ES. Tri sur un champ `text` ❌ ; `missing`, `mode`, `nested`, `unmapped_type`, tri par script ❌ |
| `_source` | ✅ | `true` / `false`, chaîne, liste, `{includes, excludes}`, motifs `*`. Aussi via `_source_includes` / `_source_excludes` en query string |
| `track_total_hits` | 🟡 | le total est **toujours exact** (`relation: "eq"`), donc `true` et une valeur numérique sont acceptés ; `false` est ❌ |
| Scoring | ✅ | BM25 (tantivy), `_score` et `max_score` renseignés ; `null` quand un tri est demandé, comme chez ES |
| Format de réponse | ✅ | `took`, `timed_out`, `_shards` (avec `failures[]` quand un index n'a pas su répondre), `hits.total.{value,relation}`, `hits.max_score`, `hits.hits[]` avec `_index` / `_id` / `_score` / `_source` / `sort` |
| `preference` | 🟡 | accepté, sans objet : il n'y a qu'un shard |
| `aggs` / `aggregations` | 🟡 | voir la section dédiée |
| `highlight`, `search_after`, `scroll`, PIT, `collapse`, `knn`, `explain`, `fields`, `post_filter`, `min_score`, `suggest`, `rescore`, `track_scores`, `q` | ❌ | |
| `ignore_unavailable`, `allow_no_indices`, `expand_wildcards` | ✅ | voir [Expressions d'index](#expressions-dindex-listes-motifs-alias) — `expand_wildcards=none` reste ❌ |
| `routing`, `filter_path`, `typed_keys` | ❌ | ferrite est mono-shard (`routing` n'a rien à choisir) ; les deux autres changent la forme de la réponse |
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
| Clauses internes | 🟡 | `term`, `terms`, `match` (sur un champ non analysé), `range`, `exists`, `prefix`, `match_all`, `match_none`, et `bool` (`must` / `filter` / `should` + `minimum_should_match` entier / `must_not`) |
| Champ `text` dans une clause interne | ❌ | les colonnes portent la valeur, pas les termes analysés. Interroger son multi-field `.keyword`, ou sortir la clause du `nested` |
| `nested` dans un `nested` | ❌ | il faudrait un indice d'élément par niveau |
| `score_mode` | 🟡 | `none` et `avg` acceptés ; le score est celui de la requête interne évaluée à plat, il n'y a pas de score par élément. Les autres modes sont ❌ |
| `inner_hits`, `ignore_unmapped` | ❌ | |
| Champs devinés sous un `nested` | ✅ | le mapping dynamique fonctionne, et la corrélation avec |
| Tri et agrégations sur un champ `nested` | ❌ | ils porteraient sur les valeurs à plat, donc sur autre chose que ce que la requête a filtré |

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
| `has_child`, `has_parent` | 🟡 | avec n'importe quelle requête interne. `score_mode` : `none` seulement (la jointure rend un score constant) |
| `parent_id` | ✅ | |
| `{"term": {"lien": "article"}}` | ✅ | le champ `join` se filtre comme un `keyword`, sous son propre nom, comme chez ES |
| `routing` | 🟡 | accepté et sans objet : il n'y a qu'un shard, donc rien à co-localiser. C'est **une contrainte d'ES en moins** |
| `inner_hits`, `min_children`, `max_children`, `ignore_unmapped` | ❌ | |

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

   En **multi-index**, la règle est resserrée pour ne pas casser un usage
   légitime : si un *autre* index visé connaît le champ, ce n'est plus une faute
   de frappe mais un mapping hétérogène, et la clause se comporte comme chez ES
   (elle ne correspond à rien, pour cet index seulement). L'erreur n'est rendue
   que si *aucun* index visé ne connaît le champ.

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
   annonce 2 dans les réponses d'écriture. En recherche multi-index, il vaut le
   **nombre d'index visés** : un index = un shard, et c'est ce que compte ES.

7. **`_cluster/health` est toujours `green`.** C'est le comportement voulu pour
   un mono-nœud : il n'y a pas de réplique à assigner.

8. **`wait_for` vaut `true` pour `refresh`.** Le commit est synchrone, il n'y a
   rien à attendre.

9. **Un sous-champ de `nested` interrogé depuis la racine est une erreur, pas 0
   résultat.** Chez Elasticsearch, ces valeurs vivent dans des documents cachés :
   `{"term": {"lignes.ref": "vis"}}` hors d'une clause `nested` ne rend **rien**,
   en silence — un piège classique. ferrite les indexe sur le document parent, il
   pourrait donc y répondre, et rendrait alors des documents là où ES n'en rend
   aucun. Il refuse explicitement, en nommant la clause `nested` attendue.

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
