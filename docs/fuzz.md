# Fuzzing différentiel — ce qui marche ailleurs que sur les cas qu'on a écrits

> L'outil : [`tests/compat/fuzz_vs_es.py`](../tests/compat/fuzz_vs_es.py).
> La mesure du jour : [`docs/fuzz.json`](fuzz.json).

## Le trou que ça bouche

Tout le reste du harnais compare ferrite à Elasticsearch sur des questions que
**nous** avons posées. La seule exception était la suite REST d'Elastic — qui
pose celles d'Elastic, mais qui est figée et date de la 7.10.

Le risque qui restait est exactement celui-là : ferrite a été construit contre
un projet réel et contre une suite de tests, et rien ne mesurait ce qui se passe
**en dehors** des combinaisons auxquelles on a pensé. Un test qu'on écrit
soi-même porte la même idée fausse que le code qu'il teste.

Le fuzzer tire au sort un mapping, des documents et des requêtes **dans le
périmètre déclaré**, les envoie aux deux serveurs, et compare les réponses
normalisées. L'oracle est un vrai Elasticsearch 8.15.

```bash
python3 tests/compat/fuzz_vs_es.py [ferrite] [es] --cas 200
python3 tests/compat/fuzz_vs_es.py --calibrer [es_a] [es_b] --cas 60
python3 tests/compat/fuzz_vs_es.py --rejouer 1234        # un cas, en détail
python3 tests/compat/fuzz_vs_es.py --couverture          # ce qu'il fuzze, et pas
```

## Le périmètre est lu, pas réécrit

[`compat.yaml`](../compat.yaml) déclare 179 capacités avec leur état. Le
générateur ne redit pas cette liste : chaque **brique** (une clause du DSL, un
type de champ, une agrégation, un paramètre du corps) cite l'identifiant de la
capacité qu'elle exerce, et au démarrage le fuzzer

- **refuse de tourner** si une brique cite un identifiant inconnu — une capacité
  renommée casse le fuzzer bruyamment plutôt que de le laisser fuzzer à côté ;
- **n'émet pas** une brique dont la capacité est déclarée `refuse` ;
- **imprime ce qu'il ne couvre pas** (`--couverture`). Un fuzzer qui ne dit pas
  où il ne va pas se lit comme s'il allait partout.

## L'étalonnage vient avant la mesure

`--calibrer` fait tourner exactement la même batterie contre **deux**
Elasticsearch. Tant qu'elle n'y est pas à zéro divergence, ce que le fuzzer dit
de ferrite ne vaut rien : une divergence peut venir du générateur (une requête
dont la réponse n'est pas déterministe) ou de la normalisation (un champ qu'on
compare alors qu'il ne peut pas coïncider) aussi bien que du moteur.

Deux serveurs sont nécessaires : la batterie **écrit**, donc l'étalonner contre
un seul mesurerait la différence entre « avant » et « après ».

L'étalonnage a servi tout de suite. Trois faux positifs sont sortis de là et
n'auraient rien dit de ferrite :

| Ce que le fuzzer signalait | Ce que c'était |
|---|---|
| l'ordre change quand `size` tronque | la troncature d'un paquet d'**ex æquo** n'a pas de réponse unique : le générateur ne tronque plus que sur un tri **total** (il finit toujours par une clé unique) |
| le tableau `sort` diffère quand on trie par `_score` | le score s'y retrouve : il est neutralisé **là aussi**, pas seulement dans `_score` |
| `sort: ["_doc"]` diffère | ES documente cet ordre comme celui du segment, pas comme une promesse d'API : le générateur ne l'émet pas |

## Ce qui est neutralisé, et pourquoi

Aucune exception n'est tacite ; la liste vit dans `NEUTRALISATIONS`, en tête du
fichier. En résumé : `took` (une durée), `_scroll_id` (un identifiant opaque),
`_ignored` (une trace d'indexation), les **valeurs** de `_score` et `max_score`
(BM25 par tantivy d'un côté, par Lucene de l'autre — c'est l'**ordre** qui est
comparé, avec la règle déjà retenue par `diff_relevance.py`), les flottants
d'une agrégation (à 1e-9 près en relatif), et le **corps** d'une erreur, pas son
statut.

Et trois choses que le générateur s'interdit, pour ne pas mesurer autre chose
que l'API : `sort: ["_doc"]`, les valeurs de `float` non représentables en
binary32, et une pagination tronquante sans ordre total.

## Les trois verdicts

Une divergence trouvée peut être trois choses, et le fuzzer ne tranche pas : il
rend le cas rejouable par `--rejouer <graine>` et imprime la requête, les deux
réponses et l'écart. C'est la mesure qui tranche.

- un **défaut de ferrite** — à corriger ;
- une **divergence assumée** — à déclarer dans `compat.yaml` ;
- une **erreur du générateur** — la requête n'a pas de réponse déterministe, ou
  elle sort du périmètre déclaré.

Les divergences assumées sont reconnues par un **prédicat écrit**, pas par un
code d'état toléré en bloc : chacun est une fonction de `DIVERGENCES_ASSUMEES`
avec sa raison, et `--tout` les imprime.

## La mesure du jour

```
400 cas, 4 942 requêtes générées, 0 divergence réelle
étalonnage (ES vs ES) : 60 cas, 738 requêtes, 0 divergence
```

Le détail machine est dans [`fuzz.json`](fuzz.json) : les divergences réelles y
sont écrites entières, les assumées résumées par famille avec trois exemples.

## Ce que le premier passage a trouvé

**Dix-sept défauts**, tous **silencieux** (ferrite répondait 200 avec un
résultat faux), tous corrigés dans la même PR. Aucun n'avait été signalé par un
client, aucun n'était couvert par le harnais existant.

| Ce qui était faux | Ce qui se passait |
|---|---|
| **tri sur un champ multivalué** | ferrite triait sur la **première** valeur ; ES trie sur le minimum en croissant et sur le maximum en décroissant. `[5, 1, 9]` se classait sur 5 au lieu de 1 |
| **valeur de tri absente** | ferrite rendait `null` ; ES rend une **sentinelle** (`9223372036854775807` sur un entier, `"Infinity"` sur un flottant) |
| **la sentinelle est une vraie valeur** | et elle se compare : un document qui porte `i64::MAX` est ex æquo avec un document qui n'a rien, et c'est la clé suivante qui les départage. ferrite mettait le document vide strictement en dernier |
| **booléen dans un `sort`** | rendu `true` / `false` ; ES rend `1` / `0` |
| **score d'un `term` sur un numérique** | noté par BM25 ; ES interroge un arbre de points et donne `1.0` à tout le monde |
| **score d'un `match` sur un numérique** | même cause, autre porte d'entrée |
| **score sur un `keyword` ou un `boolean`** | noté avec *fieldnorm*, donc un document dont le champ portait trois valeurs marquait moins qu'un autre. Chez Lucene les deux sont indexés sans norme |
| **agrégation `range` à intervalles non contigus** | tantivy **comble les trous** : un bucket `10.0-1000.0` que personne n'a demandé s'ajoutait, et décalait tous les suivants |
| **agrégation `range` en `keyed`** | la clé de la map était celle de tantivy (`-100-1000`), pas celle d'ES (`-100.0-1000.0`), et le bucket répétait un champ `key` qu'ES n'y met pas |
| **`histogram` / `date_histogram` en `keyed`** | mêmes clés de map fausses (`-1000` au lieu de `-1000.0` ; l'epoch au lieu de la date lisible) |
| **agrégation `range` sur un champ `date`** | les bornes partaient telles quelles à tantivy, qui compte en **nanosecondes** : `1767398400000` était lu comme 29 minutes après l'époque et tous les buckets sortaient vides |
| **`terms` sur un champ `date`** | la clé était une chaîne ISO ; ES rend les millisecondes plus un `key_as_string` |
| **`value_count` sur un champ `date`** | « 3 documents » devenait `3e-06`, avec un `value_as_string` à l'époque Unix |
| **`value_count`** | rendu en flottant (`3.0`) ; ES rend un entier |
| **`stats` sur un bucket vide** | ferrite ajoutait un `sum_as_string` à `1970-01-01` ; une somme de zéro date n'est pas l'époque Unix, et ES ne le rend pas |
| **`*_as_string` d'une moyenne de dates** | arrondi au plus proche ; ES **tronque** (une moyenne de `0.5` s'affiche `"0"`) |
| **`range` sur un champ `boolean`** | **500** `internal_server_error` — le `RangeQuery` de tantivy refuse un booléen |
| **`doc_count_error_upper_bound`** | toujours `0` ; ES rend `-1` quand l'ordre est `_count` croissant et que le nombre de termes distincts atteint `shard_size` |
| **deux agrégations homonymes à deux niveaux** | les métadonnées de mise en forme étaient rangées **par nom** : un `date_histogram` nommé `x` héritait de la mise en forme du `range` nommé `x` de l'autre branche, et rendait zéro bucket |

Cinq autres écarts ont été transformés en **refus explicites** plutôt qu'en
résultats faux, et déclarés dans [`compat.md`](compat.md) :

- `histogram`, `date_histogram` et `range` sur un champ **multivalué** —
  l'agrégation de tantivy compte les **valeurs**, ES compte les **documents**.
  Le refus n'est prononcé que si la colonne est réellement multivaluée : le cas
  courant reste servi et exact ;
- une agrégation `range` dont les intervalles se **chevauchent** ;
- un **trou** entre deux intervalles d'un `range` sur un champ `date` — le
  bucket de remplissage de tantivy y avale l'intervalle demandé ;
- `min_doc_count: 0` sur un `terms` — tantivy ne l'honore de façon fiable ni sur
  une colonne numérique, ni quand la requête ne ramène rien, ni dans les buckets
  vides, qui perdent alors leurs sous-agrégations ;
- `tie_breaker` sous un `multi_match` de type `most_fields`, qu'ES accepte sans
  effet.

Et une trouvaille qui n'est pas un défaut de ferrite : **ES 8.15 lui-même casse**
sur un champ `date` déclaré `format: epoch_millis` dès qu'une valeur sentinelle
apparaît (un `sort` sur un document sans valeur, un `stats` sur un bucket vide).
Il rend 400 ou 500 là où ferrite rend 200 et une réponse correcte.

Enfin, une **erreur du générateur** — le troisième verdict, et il fallait
pouvoir le distinguer : un champ laissé au mapping dynamique était modélisé
comme un `keyword`, alors qu'ES en devine un `text` **plus** un sous-champ
`.keyword`. Le générateur posait donc des `range` sur ce qui est en fait un
`text`, hors périmètre, et c'était le fuzzer qui avait tort.

## Ce qui reste, et pourquoi

Six divergences sont **assumées et déclarées**. Chacune a son prédicat dans le
fuzzer, écrit à partir d'une mesure :

| Divergence | Pourquoi elle reste |
|---|---|
| **`float` à l'affichage** | ES stocke un `float` sur 32 bits et l'imprime au plus court (`2894.4688`) ; ferrite le traduit en `f64` et l'imprime entier (`2894.46875`). Le fuzzer **vérifie** que les deux désignent le même flottant 32 bits |
| **`avgdl` de BM25** | Lucene calcule la longueur moyenne sur les documents **qui ont le champ**, tantivy sur **tous**. Dès qu'un champ `text` est facultatif, deux scores voisins peuvent s'inverser |
| **score d'un `fuzzy`** | tantivy le rend constant ; Lucene pondère chaque terme par sa distance d'édition |
| **ordre par `_score` sous un `nested`** | ferrite évalue la requête interne à plat, il n'a pas de score par élément |
| **`exists` sur un `text` sans terme** | ES tient un `_field_names` ; ferrite lit l'index inversé, où `""` n'a rien laissé. Le corriger demanderait de stocker les valeurs de chaque champ `text` une seconde fois |
| **court-circuit d'ES** | un `bool` dont une clause obligatoire est `match_none` ne rend rien : ES s'arrête là et ne voit jamais qu'une autre clause est malformée. ferrite valide la requête entière |

La ligne des divergences de pertinence est étroite **exprès** : elle n'accepte
un ordre différent que si ES lui-même donne deux scores **différents** aux
documents échangés. Si ES les classe ex æquo, l'inversion ne vient pas de BM25
mais d'une clé de tri — et c'est par là qu'ont été trouvés le `term` sur un
numérique et le tri sur un champ multivalué. Aucun des deux n'aurait été masqué.

## Une graine se rejoue — avec la version du générateur

`--rejouer <graine>` reconstruit le mapping, les documents et les requêtes du
cas, et imprime tout. C'est vrai **à générateur constant** : changer le tirage
(ajouter une brique, changer une probabilité) décale les graines. Une divergence
qui compte ne reste donc pas une graine — elle devient un cas figé dans le
harnais, ici [`tests/compat/sonde_fuzz.py`](../tests/compat/sonde_fuzz.py).
