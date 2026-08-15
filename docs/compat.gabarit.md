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

<!-- table:legende -->

Un ❌ porte toujours son **motif**, parce que « je ne sais pas encore faire » et
« je refuse exprès » ne se corrigent pas de la même façon :

<!-- table:motifs -->

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

<!-- table:ponderation -->

---

## Poignée de main et cluster

<!-- table:cluster -->

## Hors périmètre déclaré

Les familles de routes qu'Elasticsearch a et que ferrite n'a pas. Elles étaient
jusqu'ici décrites en une phrase du README (« sharding, réplication, consensus…
Painless ») : elles sont désormais **déclarées**, une famille à la fois, avec
son motif. C'est ce qui permet au rapport de conformance de trancher — un cas
qui échoue sur `_snapshot` n'est pas le même événement qu'un cas qui échoue sur
`_search`.

<!-- table:hors_perimetre -->

C'est de loin la famille d'écarts la plus fournie de la suite de conformance
d'Elastic : l'écrasante majorité de ses échecs est un `no handler found for uri
[...]`, c'est-à-dire une route qu'ES a et que ferrite n'a pas. Le compte du jour
se lit dans [`conformance.json`](conformance.json), qui range désormais chaque
échec en **régression** (une capacité déclarée supportée) ou en **coût de
périmètre** (une capacité déclarée refusée).

## Index et mapping

<!-- table:index -->

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

<!-- table:analyzers -->

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

<!-- table:analyzers_sur_mesure -->

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

<!-- table:types_champ -->

## Ingestion

<!-- table:ingestion -->

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

<!-- table:scroll -->

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

<!-- table:expressions_index -->

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

<!-- table:alias -->

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

<!-- table:alias_conformance -->

## Réglages de cluster

<!-- table:reglages_cluster -->

Conséquence : `DELETE /audits-2026.07.*` et `DELETE /_all` sont **refusés par
défaut**, avec le message d'ES (`Wildcard expressions or all indices are not
allowed`). C'est délibéré : un projet qui purge par motif a forcément basculé ce
réglage sur son Elasticsearch, et si ferrite obéissait là où ES refuse, la
première différence de comportement entre les deux serveurs serait une
suppression de données.

### Clauses du Query DSL

<!-- table:dsl -->

### La recherche libre (`multi_match`)

C'est la clause d'une barre « chercher par référence / nom / montant » : la même
chaîne posée sur plusieurs champs, souvent de **types différents**. Deux
paramètres y sont indispensables et manquaient, tous deux signalés par le
premier client de ferrite.

<!-- table:recherche_libre -->

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

Les bords, tous mesurés contre un vrai ES 8.15
([`tests/compat/sonde_msm.py`](../tests/compat/sonde_msm.py), **47/47
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

<!-- table:corps_search -->

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

<!-- table:datemath -->

Une expression malformée est refusée avec **le message d'ES, mot pour mot**
(`unit [q] not supported for date math [-1q]`, `truncated date math [/]`,
`operator not supported for date math [1d]`, `For input string: "…"`). ES les
rend sous un `search_phase_execution_exception` « all shards failed » dont la
`root_cause` porte ce texte ; ferrite rend l'erreur directement, sans cet
empilement.

## Agrégations

Comparées champ par champ à un vrai ES 8.15 sur 45 requêtes
(`tests/compat/diff_aggs.py`), clés de réponse comprises.

<!-- table:aggs -->

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

<!-- table:nested -->

### `join` (parent/enfant)

Parent et enfant sont deux documents distincts, réunis à la requête.
`has_child` / `has_parent` s'évaluent en **deux passes** : la requête interne est
exécutée, les identifiants qui en sortent deviennent une recherche sur `_id` (ou
sur la colonne du parent). Exact, et borné par le nombre d'identifiants
distincts. Elasticsearch a besoin de *global ordinals* pour ça parce qu'il est
distribué ; mono-shard, parent et enfant sont forcément au même endroit.

<!-- table:join -->

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
    ([`tests/compat/sonde_vide.py`](../tests/compat/sonde_vide.py), 27/27
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
