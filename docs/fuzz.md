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

Une capacité déclarée tenue mais qu'aucune brique n'exerce sort **déclarée sans
être mesurée** : c'est pour ça que la liste des briques grandit avec le
périmètre. Trois routes de description y sont entrées avec les leurs, et chacune
porte un prédicat écrit sur ce qui n'est **pas** comparé :

| Brique | Ce qui est comparé | Ce qui ne l'est pas, et pourquoi |
|---|---|---|
| `_field_caps` sur le mapping tiré au sort | le type, `searchable`, `aggregatable`, la liste `indices`, champ par champ | les champs de **métadonnées** (`_id`, `_index`, `_seq_no`…) : ferrite ne les expose pas, et c'est déclaré — il ne sait pas les interroger |
| `_validate/query` sur chaque requête générée | le verdict `valid` | l'`explanation` : celle d'ES est la chaîne Lucene, celle de ferrite le rendu de sa requête tantivy. Et un `valid: false` là où ES dit `true` n'est un écart **de cette route** que si ferrite accepte pourtant la requête en recherche — sinon c'est le refus que la comparaison de recherche vient de mesurer, vu d'ailleurs |
| `_stats` | `docs.count` | `store.size_in_bytes` : deux moteurs de stockage. Et `docs.count` lui-même diverge dès qu'il y a du `nested` — Lucene compte ses sous-documents, ferrite n'en a pas. Le prédicat le **mesure** : il exige que le compte de ferrite égale ce que la recherche rend des deux côtés, et que celui d'ES lui soit strictement supérieur |

Une brique de plus ne pose pas de requête : une fois sur quatre, le mapping
n'est pas posé sur l'index mais dans un **template**, et l'index naît de
l'écriture. La comparaison de mapping qui suit mesure alors ce qu'un template
applique vraiment, sur un mapping que personne n'a choisi.

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
graines 1–400          400 cas, 16 714 requêtes, 0 divergence réelle
graines 5000–5299      300 cas, 12 475 requêtes, 0 divergence réelle
graines 900000+        250 cas, 10 441 requêtes, 1 divergence réelle  (ouverte, décrite plus bas)
graines 4242000+       250 cas, 10 417 requêtes, 0 divergence réelle
graines 31337000+      250 cas, 10 395 requêtes, 0 divergence réelle
graines 7770000+       250 cas, 10 413 requêtes, 0 divergence réelle
graines 6060000+       250 cas, 10 400 requêtes, 0 divergence réelle
                     ------------------------------------------------
                     1 950 cas, 81 255 requêtes, 1 divergence réelle

étalonnage ES vs ES     60 cas,  2 418 requêtes, 0 divergence
```

Deux de ces plages ont servi à corriger : 1–400, contre laquelle l'outil a été
réglé, et 4242000+, qui a sorti le `minimum_should_match` décrit plus bas. Les
cinq autres sont des plages **de contrôle**, et **6060000+ n'a jamais été
regardée avant ce passage** : c'est son zéro qui mesure ferrite plutôt que mon
itération. Le rapport machine ([`fuzz.json`](fuzz.json)) est le sien.

Les graines ont toutes changé de sens depuis la mesure précédente : le
générateur pose maintenant, en plus, un tri et une agrégation sur un sous-champ
de `nested`. Chaque cas de chaque plage est donc un tirage différent — c'est
exactement pour ça que ce qui compte devient un cas écrit dans
[`sonde_fuzz.py`](../tests/compat/sonde_fuzz.py) et pas une graine.

### Les deux divergences du passage précédent : corrigées

Elles avaient été publiées ouvertes plutôt que tues. Les voici, avec ce qu'elles
sont devenues.

| Ce qui différait | Ce que c'est devenu |
|---|---|
| **Un bucket vide de `histogram` n'avait pas ses sous-agrégations.** tantivy comble les trous entre deux extrêmes, mais ses buckets de remplissage n'exécutent aucune sous-agrégation : un `range` sous un `histogram` y rendait `buckets: []` là où ES rend ses trois intervalles à `doc_count: 0` | **corrigé** dans `src/aggs.rs`. Un bucket à `doc_count: 0` ne contient rien : ses sous-agrégations sont donc, mot pour mot, celles d'une recherche qui ne ramène rien. Cette forme-là n'est pas écrite à la main, elle est **mesurée** — les sous-agrégations de chaque `histogram` sont rejouées sur une requête vide, et le bucket prend cette réponse. `diff_aggs.py` passe de 45 à 53 cas, les huit nouveaux étant précisément ceux-là |
| **Une agrégation sur un sous-champ de `nested` depuis la racine.** ES n'y voit aucun document et rend son résultat vide (`null`, `0.0`, `buckets: []`) ; ferrite agrégeait les valeurs à plat et rendait un autre nombre — mesuré : `avg` de `7.0` là où ES rend `null` | **refusée**, comme la requête équivalente l'était déjà (divergence assumée n° 10 de `compat.md`) : un refus se voit, un nombre faux non. En mesurant le voisinage, le **tri** sur le même chemin s'est révélé porter le même défaut — et là c'est ES qui refuse (`it is mandatory to set the [nested] context on the nested sort field`) pendant que ferrite rendait un ordre en 200 |

Les deux rendaient 200 avec un résultat faux, ce qui est précisément la famille
que ce projet cherche à ne pas laisser passer. Elles sont figées dans
[`sonde_fuzz.py`](../tests/compat/sonde_fuzz.py), qui passe de 35 à 42 cas et de
6 à 10 refus, et le générateur pose maintenant les deux formes exprès : quand le mapping
tiré au sort contient un `nested`, une agrégation sur un de ses sous-champs sort
une fois sur douze et un tri sur un de ses sous-champs une fois sur douze aussi
(mesuré sur 400 mappings : 66 agrégations et 45 tris). Une correction que plus
personne n'exerce se défait en silence.

Une note de méthode, parce qu'elle a coûté une demi-heure : la deuxième
divergence était attribuée ici à une graine (900119) qui, rejouée, montre tout
autre chose — un `sum` sur un champ **objet**, pas `nested`. Le défaut `nested`
était bien réel (il se mesure en trois lignes contre ES), mais la ligne qui le
rattachait à cette graine était fausse. Une divergence ne se range pas sous une
graine : elle se réduit à un cas écrit. C'est exactement ce que dit la dernière
section de ce fichier, et c'est la deuxième fois que le dépôt le paie.

### Ce que la campagne de ce passage a trouvé en plus

Une plage de contrôle a sorti un **troisième** résultat faux rendu en 200, sans
rapport avec les deux précédents — c'est exactement ce à quoi sert une plage
qu'on n'a jamais regardée.

| Ce qui différait | Ce que c'est devenu |
|---|---|
| **Sous un `nested`, un `minimum_should_match` explicite qui retombe à zéro jetait le `should` entier.** `"50%"` d'une seule clause vaut `0` (la troncature vers zéro d'ES), et ferrite en concluait « pas de minimum, donc pas de clause ». Un document dont un élément satisfaisait seulement le `must_not` remontait, là où ES n'en rend aucun | **corrigé**. Lucene exige au moins une clause positive quand aucune clause obligatoire n'est là, quel que soit le minimum demandé : la règle s'applique **après** la résolution du paramètre, pas à sa place. La correction précédente ne portait que sur la valeur **par défaut** du paramètre — corriger le défaut d'un paramètre ne corrige pas le paramètre. Figé dans `sonde_msm.py`, qui passe de 47 à 53 cas |

### Ce que la graine 900119 montrait vraiment, et qui reste ouvert

| Ce qui diffère | Ce que c'est |
|---|---|
| **`sum` d'entiers hors du domaine exact d'un `double`.** Sur un corpus qui contient `-9223372036854775808`, `9223372036854775807`, `-1` et `1`, ES rend `0.0` et ferrite `-1.0` | ES accumule sa somme en **double** — `-2^63 + 2^63` y vaut exactement `0`, puis `-1`, puis `0` ; tantivy accumule en `i64`, donc exactement : `-1`, `-2`, `-1`. Les deux sont défendables, et celle de ferrite est arithmétiquement juste ; mais ce n'est pas celle d'ES. L'écart n'existe qu'au-delà de 2^53, là où un `double` ne représente plus tous les entiers |

Elle est **ouverte** : elle ne se corrige pas dans la couche de mise en forme
(la somme est faite par le collecteur de tantivy), et elle n'est pas de la
famille traitée par cette carte. Elle est publiée ici pour la même raison que
les deux précédentes — c'est le seul usage honnête de cet outil.

Le détail machine est dans [`fuzz.json`](fuzz.json) : les divergences réelles y
sont écrites entières, les assumées résumées par famille avec trois exemples.

### Pourquoi cinq plages de graines, et pas une

Parce que la première ne prouvait pas ce qu'elle avait l'air de prouver.

Les graines 1–400 sont celles contre lesquelles l'outil a été **réglé** : à
chaque divergence corrigée, à chaque prédicat écrit, c'est sur elles que je
relançais. Le zéro qu'elles ont fini par afficher était donc en partie du
**surajustement** — pas une propriété de ferrite, une propriété de mon
itération.

La preuve : chaque nouvelle plage jamais regardée en a retrouvé.

| Plage | Ce qu'elle a sorti |
|---|---|
| 5000–5299 | **sept** divergences, dont une vraie — `sum_other_doc_count` faux dès que `min_doc_count` dépasse 1 — et deux prédicats trop étroits (ci-dessous) |
| 900000+ | **deux** défauts réels : `fuzzy` sur un champ `date` ou numérique rendait « zéro document » en 200, et le score d'un `bool` purement négatif valait le `boost` au lieu de `0.0` |
| 4242000+ | un trou du **rapport**, pas du moteur : un `scroll` refusé ne transportait pas le motif de son refus, donc un refus déclaré s'y lisait comme une divergence réelle |
| 31337000+ | rien de neuf — la première plage de contrôle qui n'ajoute rien |

Et au passage suivant, générateur changé, la règle a rejoué exactement pareil :
la plage 1–400 — celle sur laquelle on avait itéré — a sorti le **troisième**
prédicat trop étroit du tableau ci-dessous, et deux plages de contrôle ont sorti
les deux divergences réelles décrites plus haut.

Les trois prédicats trop étroits :

| Ce qui manquait | Ce que ça a coûté |
|---|---|
| le court-circuit d'ES était reconnu à son **type d'erreur** | trois autres refus légitimes de ferrite comptés comme des divergences. Le prédicat regarde maintenant si la requête est court-circuitable, ce qui est la propriété qui explique l'écart |
| `exists` sur un `text` était reconnu à un **compte de documents** | sous un `bool { should: [exists], filter: […] }`, le manque ne se voit que dans le **score** : ES donne 1.0, ferrite 0.0, et aucun compte ne bouge. Le fuzzer repose maintenant la clause `exists` seule aux deux serveurs pour trancher — il le **mesure**, il ne le suppose pas |
| le court-circuit d'ES n'était reconnu que sur le chemin `statut` d'une **recherche** | le même court-circuit se produit à l'ouverture d'un `scroll`, et l'écart s'y lisait comme réel. Un prédicat qui nomme un chemin plutôt qu'une propriété finit toujours par manquer une route |

D'où la règle : **une plage de graines sur laquelle on a itéré ne mesure plus
rien.** Il en faut une qu'on n'a jamais regardée, et la publier séparément.

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
