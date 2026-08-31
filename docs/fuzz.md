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

[`compat.yaml`](../compat.yaml) déclare 200 capacités avec leur état. Le
générateur ne redit pas cette liste : chaque **brique** (une clause du DSL, un
type de champ, une agrégation, un paramètre du corps) cite l'identifiant de la
capacité qu'elle exerce, et au démarrage le fuzzer

- **refuse de tourner** si une brique cite un identifiant inconnu — une capacité
  renommée casse le fuzzer bruyamment plutôt que de le laisser fuzzer à côté ;
- **n'émet pas** une brique dont la capacité est déclarée `refuse` ;
- **imprime ce qu'il ne couvre pas** (`--couverture`). Un fuzzer qui ne dit pas
  où il ne va pas se lit comme s'il allait partout.

Une capacité déclarée tenue mais qu'aucune brique n'exerce sort **déclarée sans
être mesurée** : c'est pour ça que la liste des briques grandit avec le
périmètre. Trois routes de description y sont entrées avec les leurs, et chacune
porte un prédicat écrit sur ce qui n'est **pas** comparé :

| Brique | Ce qui est comparé | Ce qui ne l'est pas, et pourquoi |
|---|---|---|
| `_field_caps` sur le mapping tiré au sort | le type, `searchable`, `aggregatable`, la liste `indices`, champ par champ | les champs de **métadonnées** (`_id`, `_index`, `_seq_no`…) : ferrite ne les expose pas, et c'est déclaré — il ne sait pas les interroger |
| `_validate/query` sur chaque requête générée | le verdict `valid` | l'`explanation` : celle d'ES est la chaîne Lucene, celle de ferrite le rendu de sa requête tantivy. Et un `valid: false` là où ES dit `true` n'est un écart **de cette route** que si ferrite accepte pourtant la requête en recherche — sinon c'est le refus que la comparaison de recherche vient de mesurer, vu d'ailleurs |
| `_stats` | `docs.count` | `store.size_in_bytes` : deux moteurs de stockage. Et `docs.count` lui-même diverge dès qu'il y a du `nested` — Lucene compte ses sous-documents, ferrite n'en a pas. Le prédicat le **mesure** : il exige que le compte de ferrite égale ce que la recherche rend des deux côtés, et que celui d'ES lui soit strictement supérieur |
| l'**écriture d'un alias** (`alias.put`, `alias.aliases`) | le statut **et quel index porte quel alias** après coup — sur les sept URL de `put_alias` tirées au sort, puis un `remove` dont l'expression (nom exact, motif, `_all`, nom absent) et le `must_exist` (absent, `true`, `false`) sont tirés au sort eux aussi | le **motif** d'un refus, comme partout ici : ferrite nomme ses refus avec ses propres mots. Une brique qui ne comparerait que le statut serait verte sur une commande qui pose l'alias sur le mauvais index |

Une brique de plus ne pose pas de requête : une fois sur quatre, le mapping
n'est pas posé sur l'index mais dans un **template**, et l'index naît de
l'écriture. La comparaison de mapping qui suit mesure alors ce qu'un template
applique vraiment, sur un mapping que personne n'a choisi.

### Ce qu'une brique exercée ne garantit pas : l'échelle

`agg.sous_agregations` est exercée depuis longtemps — une agrégation de buckets
sur deux porte des sous-agrégations tirées au sort. Ça n'a pas empêché le
défaut le plus grave qu'ait connu ce projet de vivre sous le fuzzer sans être
vu : une sous-agrégation perdait les documents de ses buckets rares au-delà de
**2 048 documents par segment**, et un cas du fuzzer en indexe **25**.

Aucune probabilité, aucune graine, aucune brique de plus n'y changerait quoi que
ce soit : le fuzzer mesure des **formes**, pas des tailles. Publier une brique
comme couverte, c'est donc dire « cette forme est comparée », pas « cette
capacité est mesurée partout où elle vit ». La taille est le quatrième angle
mort structurel de ce harnais, après celui des applications réelles (elles
commencent par créer leur index), celui de la taille des corps HTTP, et celui
des tailles de corpus — et c'est le banc à l'échelle qui le couvre, pas ce
fichier. La mesure de celui-là vit dans
[`sonde_sous_aggs.py`](../tests/compat/sonde_sous_aggs.py) et son histoire dans
[`tantivy-patch.md`](tantivy-patch.md).

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

Le générateur a changé — cinq briques de plus, **le réglage de la pertinence** :
`q.function_score` (les deux formes du corps, celle à fonction unique et celle
à `functions[]`), `q.function_score.valeur` (un `field_value_factor` avec son
`factor`, son `modifier` et son `missing`), `q.function_score.decroissance`
(`gauss` / `exp` / `linear`, sur un numérique comme sur une date),
`q.function_score.bornes` (`max_boost` et `min_score`) et `q.boosting`. Donc
**toutes les graines ont changé de sens** : la campagne précédente ne mesurait
plus les mêmes cas, et ses chiffres ne sont pas reconduits.

Ce tableau est celui de ce passage, sur des plages **jamais utilisées pour
corriger** — les quatre plages qui ont servi à trouver les défauts plus bas
(`5100000+`, `7300000+`, `8810000+`, `9420000+`) sont publiées à part, parce
qu'une plage sur laquelle on a itéré ne mesure plus rien.

```
plages de contrôle, jamais regardées avant ce tableau
graines 1610000+       120 cas,  5 565 requêtes, 0 divergence
graines 2730000+       120 cas,  5 577 requêtes, 0 divergence
graines 3840000+       120 cas,  5 693 requêtes, 1 divergence  (ouverte, plus bas)
                     ------------------------------------------------
                       360 cas, 16 835 requêtes, 1 divergence

les mêmes plages, contre le binaire d'AVANT la carte
                       360 cas, 16 914 requêtes, 383 divergences
                                 (118 + 129 + 136)

les quatre plages sur lesquelles on a itéré, une fois corrigées
                       480 cas, 22 211 requêtes, 0 divergence
```

Les deux colonnes sont mesurées avec des binaires **release** des deux côtés,
comme les fois précédentes.

La ligne du milieu dit que les briques mesurent quelque chose, et elle se lit
avec la même réserve qu'à chaque carte : `function_score` et `boosting` étaient
**refusés** avant, donc chaque requête qui en tire un rendait 400 d'un côté et
200 de l'autre. Ce 383 mesure que les briques sont posées souvent, pas qu'elles
ont trouvé 383 défauts.

### Ce que ce passage a trouvé : quatre défauts, tous silencieux

Trois d'entre eux sont le **même** défaut vu par trois chemins, et aucun n'était
visible à la grille de 47 000 points qui verrouille pourtant les formules.

| Graine | Ce qui était faux | Ce que c'était |
|---|---|---|
| `7300048`, `7300100` | ferrite rend **200** avec un classement inventé là où ES refuse en **500** (`function score query returned an invalid score: NaN`) | `Math.min` de Java **propage** `NaN` ; le `f64::min` de Rust rend l'autre opérande. Un score de fonction `NaN` — ce que produit un `sqrt` sur une valeur négative ou un `log1p` sous -1, donc un `missing: -1` — traversait `min(fonction, max_boost)` **en devenant le plafond**. La grille ne portait ni `NaN` ni les infinis : elle en porte maintenant, et elle compte 58 476 points |
| `5100036` | même famille, sous un `score_mode: avg` et un `boost_mode: replace` | même correction |
| `8810020` | `min_score: 1` avec `boost: 2` : ferrite garde 6 documents, ES 4, dès qu'un `sort` remplace le score | le `boost` d'une clause n'est appliqué **que si le collecteur demande des scores** — Lucene comme tantivy laissent alors tomber leur `BoostQuery`. Invisible partout ailleurs (un facteur constant ne change pas un ensemble de documents), décisif sous un `min_score`, qui fait du score un **seuil**. Deux conséquences : le `boost` est porté **dans** la clause (le `Weight::count` de tantivy reconstruit le scorer à boost 1.0, ce qui rendait un `total` plus petit que le nombre de hits) et le total d'une recherche libre se compte avec un collecteur qui demande les scores — sauf à `size: 0`, où personne n'en lit, exactement comme ES |
| `5100002`, `5100005`, … | `_validate/query` rend `valid: false` sur des requêtes qu'ES déclare valides | une décroissance sur un champ inconnu est un verdict de **mapping**, et ES lui donne pourtant le type `parsing_exception`. Contre le schéma vide de la validation, tout champ est inconnu — le refus est donc marqué « échec de shard », et c'est cette marque, pas le type, qui le sépare d'une erreur de forme. Le même piège que sur `nested`, deux cartes plus tôt |
| `5100022` | aucun fragment surligné sous un `function_score` ni sous un `boosting` | le parcours de `highlight` ne connaissait pas les deux clauses. `function_score` marque ce que **sa requête** a fait correspondre (pas ses fonctions, pas les `filter` de ses `functions[]`), `boosting` ne marque que son `positive` — et un `min_score`, même à zéro, fait **taire tout le sous-arbre** : ES y perd ses `Matches`. Une brique nouvelle ne mesure pas qu'elle-même, une fois de plus |

Les neuf cas correspondants sont figés dans
[`sonde_fuzz.py`](../tests/compat/sonde_fuzz.py), hors d'une graine — et les
neuf échouent contre le binaire d'avant, ce qui est la seule façon de savoir
qu'ils mesurent quelque chose.

### La divergence ouverte de ce passage

| Graine | Ce qui diffère | Ce que c'est |
|---|---|---|
| `3840041` | ferrite refuse en 400 (`include` posé sur un `long` dans un `terms`, un refus qu'il déclare), ES échoue en **500** (`Index 0 out of bounds for length 0`) | les deux serveurs refusent, avec leurs propres mots. Le prédicat « refus déclaré » exige que l'autre côté **réponde** (`droite 200`) — sans cette moitié, n'importe quel 500 d'ES passerait pour un coût de périmètre de ferrite, et c'est précisément l'élargissement discret contre lequel ce dépôt s'est déjà fait avoir. La ligne rouge est le bon prix. Elle sort **à l'identique contre le binaire d'avant** : elle n'a rien à voir avec cette carte |

Et une divergence de plus est **absorbée** par un prédicat nouveau, qui
**mesure** au lieu de supposer : un `min_score` posé sur une requête dont le
score de base n'est déjà pas le même des deux côtés. `min_score` est le seul
endroit où le score cesse d'être un ordre pour devenir un seuil, et ferrite et
ES ne calculent pas le même BM25 dès qu'un champ `text` est facultatif — c'est
l'`avgdl`, déjà déclaré dans [`compat.md`](compat.md) (0,998 chez ES contre
1,169 chez ferrite sur le même document, de part et d'autre d'un
`min_score: 1`). Le prédicat ne le suppose pas : il **repose la sous-requête
seule** aux deux serveurs et n'accepte l'écart que si leurs scores y diffèrent
déjà. Un vrai défaut de `min_score` laisse les scores de base identiques, et ne
passe donc pas.

## Ce que le premier passage a trouvé

**Vingt et un défauts**, tous **silencieux** (ferrite répondait 200 avec un
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
| **`fuzzy` sur un champ `date` ou numérique** | ferrite construisait un terme texte sur une colonne qui n'en contient pas et rendait **zéro document en 200** ; ES refuse. Un résultat vide qui se fait passer pour une réponse |
| **`prefix` sur un champ non textuel, sous un `nested`** | la vérification du type de champ existait à la racine et manquait dans la branche `nested` |
| **score d'un `bool` purement négatif** | ES donne `0.0` aux documents qu'un `bool` sans clause positive laisse passer, quel que soit son `boost` ; ferrite leur donnait le score de la clause positive implicite (`1.5` sous un `boost: 1.5`), et l'ordre changeait dès que ce `bool` était combiné à autre chose |

Cinq autres écarts ont été transformés en **refus explicites** plutôt qu'en
résultats faux, et déclarés dans [`compat.md`](compat.md) :

- `histogram`, `date_histogram` et `range` sur un champ **multivalué** —
  l'agrégation de tantivy compte les **valeurs**, ES compte les **documents**.
  Le refus n'est prononcé que si la colonne est réellement multivaluée : le cas
  courant reste servi et exact ;
- une agrégation `range` dont les intervalles se **chevauchent** ;
- un **trou** entre deux intervalles d'un `range` sur un champ `date` — le
  bucket de remplissage de tantivy y avale l'intervalle demandé ;
- `min_doc_count` autre que sa valeur par défaut sur un `terms` — à `0`, tantivy
  ne l'honore de façon fiable ni sur une colonne numérique, ni quand la requête
  ne ramène rien, ni dans les buckets vides, qui perdent alors leurs
  sous-agrégations. Au-delà de `1`, c'est `sum_other_doc_count` qui ne suit plus,
  et **c'est le seul endroit où j'ai renoncé après avoir vraiment cherché** :
  une formule ajustée sur quinze formes d'un corpus les collait toutes, puis
  s'est effondrée sur d'autres (27 écarts sur 1 450 cas tirés au sort). La règle
  d'ES dépend de l'ordre demandé, de la troncature et de l'ordre de parcours du
  dictionnaire de termes — c'est son collecteur qu'il faudrait réécrire, et
  annoncer un compte faux serait pire. Le cas de `diff_aggs.py` qui l'utilisait
  a été ramené à la valeur par défaut, dans la même PR ;
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

Sept divergences sont **assumées et déclarées**. Chacune a son prédicat dans le
fuzzer, écrit à partir d'une mesure :

| Divergence | Pourquoi elle reste |
|---|---|
| **`float` à l'affichage** | ES stocke un `float` sur 32 bits et l'imprime au plus court (`2894.4688`) ; ferrite le traduit en `f64` et l'imprime entier (`2894.46875`). Le fuzzer **vérifie** que les deux désignent le même flottant 32 bits |
| **`avgdl` de BM25** | Lucene calcule la longueur moyenne sur les documents **qui ont le champ**, tantivy sur **tous**. Dès qu'un champ `text` est facultatif, deux scores voisins peuvent s'inverser |
| **score d'un `fuzzy`** | tantivy le rend constant ; Lucene pondère chaque terme par sa distance d'édition |
| **ordre par `_score` sous un `nested`** | ferrite évalue la requête interne à plat, il n'a pas de score par élément |
| **`exists` sur un `text` sans terme** | ES tient un `_field_names` ; ferrite lit l'index inversé, où `""` n'a rien laissé. Le corriger demanderait de stocker les valeurs de chaque champ `text` une seconde fois |
| **court-circuit d'ES** | un `bool` dont une clause obligatoire est `match_none` ne rend rien : ES s'arrête là et ne voit jamais qu'une autre clause est malformée. ferrite valide la requête entière |
| **chemin d'ordre non vérifié par ES** | le même court-circuit, sur un autre chemin de code, et c'est ce passage qui l'a trouvé. ES ne résout le chemin d'ordre d'un `terms` qu'au moment de comparer deux seaux : à zéro ou un seul seau, il ne trie rien et **ne valide rien** — il rend 200 sur une agrégation d'ordre qui n'existe pas, sur un `stats` sans clé, sur une propriété qu'aucune métrique ne rend. `size: 1` ne suffit pas (il collecte tous les seaux et ne tronque qu'après) : c'est bien le nombre de seaux **retenus** qui décide. ferrite valide avant d'exécuter — sinon la même requête serait tantôt acceptée tantôt refusée, selon le jeu de données. Voir la divergence assumée n° 23 |

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
