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

[`compat.yaml`](../compat.yaml) déclare 197 capacités avec leur état. Le
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

Le générateur a changé — trois briques de plus (`search_analyzer`, `copy_to`,
`store`) — donc **toutes les graines ont changé de sens** : la campagne
précédente ne mesurait plus les mêmes cas, et ses chiffres ne sont pas
reconduits. Ce tableau est celui de ce passage, refait en entier.

```
graines 5150000+       250 cas, 11 020 requêtes, 0 divergence réelle
graines 6260000+       250 cas, 11 131 requêtes, 0 divergence réelle
graines 7370000+       250 cas, 11 037 requêtes, 0 divergence réelle
graines 3535000+       250 cas, 10 959 requêtes, 0 divergence réelle
graines 9494000+       250 cas, 11 098 requêtes, 0 divergence réelle
graines 4242000+       250 cas, 11 082 requêtes, 1 divergence ouverte
graines 1717000+       250 cas, 11 000 requêtes, 0 divergence réelle
graines 2626000+       250 cas, 11 040 requêtes, 0 divergence réelle
                     ------------------------------------------------
                     2 000 cas, 88 367 requêtes, 1 divergence ouverte

étalonnage ES vs ES     50 cas,  2 145 requêtes, 0 divergence
```

Trois de ces plages ont servi à corriger : **5150000+**, qui a montré que la
valeur stockée écrasait la valeur formatée de `fields` ; **4242000+**, qui a
sorti le budget de `max_expansions` ; et **1717000+**, qui a sorti la limite des
tokenizers de Lucene — une plage jamais regardée avant ce passage, comme
**2626000+**. Les cinq autres sont des plages de contrôle de ce passage-ci :
elles n'ont servi à rien corriger. Le rapport machine publié est celui de la
dernière, [`fuzz.json`](fuzz.json).

Et la règle a rejoué exactement comme les fois précédentes : **les deux plages
neuves en ont sorti une chacune**, et aucune n'était dans le sujet de la carte.
Six passages, aucune plage neuve muette.

**La divergence de 4242000+ est ouverte, pas corrigée.** C'est celle que ce
document décrit plus bas : un ordre par `_score` que BM25 sépare et qu'ES rend
ex æquo, sur un champ `text` **facultatif** — l'`avgdl` de Lucene se calcule sur
les documents qui ont le champ, celui de tantivy sur tous. Le prédicat refuse de
l'absorber, exprès ; élargir la ligne masquerait exactement ce qu'elle a été
écrite pour attraper.

### Ce que la brique « surlignage » a sorti

La brique pose un bloc `highlight` sur un quart des recherches : un ou deux
champs (nommés, en motif, ou `*`), des tailles **petites exprès** — c'est sous
`fragment_size` que deux lectures du découpeur divergent, au-dessus le fragment
est la phrase entière et n'importe quelle implémentation tombe juste — et, une
fois sur cinq, une surcharge champ par champ.

Elle a sorti **six** défauts en trois passages, et aucun n'était visible aux
192 questions écrites à la main la veille. Le premier a coûté un changement de
forme du code :

| Ce que le fuzzer a sorti | Ce que c'était |
|---|---|
| un `should` placé dans un `bool` dont le `filter` échoue était quand même marqué | **« les termes de la requête » n'est pas « ce qui a fait correspondre ce document ».** ES surligne depuis les `Matches` de Lucene, qui sont calculées **par document** : un `bool` qui ne tient pas ne rend aucune marque, et un `must_not: {match_all}` le réécrit en `MatchNoDocsQuery`. Le surlignage garde donc la forme booléenne de la requête et l'évalue document par document. La règle a une seconde moitié qui la retourne : sous `require_field_match: false`, ES abandonne les `Matches` et repart d'une extraction statique — le tri par document disparaît avec, et une phrase y est marquée terme par terme |
| `no_match_size: 5` rendait « tiret- » là où ES rend « tiret-bas » | **un `BreakIterator` de Java n'est pas UAX#29.** `abcde-fghij` et `abcde"fghij` sont **un** mot chez Java, `abcde:fghij` et `abcde’fghij` en font deux — l'inverse de la norme pour les deux premiers. Ça ne se lit nulle part : il a fallu poser `no_match_size: 1` sur seize mots construits exprès et regarder où tombait la coupure |
| un `match` sur un `keyword` ne surlignait plus rien dès que la valeur portait un tiret | **un `keyword` n'est pas analysé par la clause** : elle y cherche la valeur entière. Le mapping ne déclare pas d'analyzer sur un `keyword`, donc l'analyzer « par défaut » d'un champ y est `standard` — et l'appliquer coupait « tiret-bas » en deux termes qui n'existent pas dans l'index |
| une valeur écartée par `ignore_above` était surlignée | **lire le `_source` n'est pas lire ce qui a été indexé** — la même leçon que pour `fields`, un an plus tôt, au même endroit. Une valeur trop longue n'est ni marquée, ni rendue par `no_match_size` |
| la cible d'un `copy_to` ne rendait aucun fragment | sa valeur n'est **nulle part** dans son propre `_source` : elle est dans celui de la source. Là encore, la règle existait déjà pour `fields` ; livrer une troisième lecture, c'est devoir la reposer |
| `<em>optique</em><em> verre</em>` là où ES rend `<em>optique verre</em>` | deux marques qui se chevauchent se trient **début croissant, fin décroissante** (`Passage.sort()` de Lucene) : le formateur avance sur la fin de la précédente, donc la plus longue doit passer d'abord |
| `no_match_size` ne rendait rien quand la première valeur d'un champ multivalué était vide | ES concatène les valeurs avec un séparateur et **saute les séparateurs de tête** : la première valeur au sens de `no_match_size` est la première **non vide** |
| un `must: {exists: b}` qui échoue laissait ses voisins marquer | `exists` ne marque rien, mais il se **tranche** sur le `_source`, et un `bool` qu'il fait tomber doit se taire entièrement. Le laisser opaque revenait à supposer qu'il tenait |
| `"  abc def  "` sortait rogné à `number_of_fragments: 0` | le rognage vit dans le **découpeur borné** ; à `nof: 0` ES ne l'emploie pas et rend le fragment tel quel. Et le rognage lui-même n'est pas « les blancs » : c'est le `String.trim()` de Java, qui s'arrête à U+0020 — l'espace insécable, l'espace fine et le séparateur de ligne restent |
| ES rendait `cible\u2009` là où ferrite rendait `cible\t` | le **score** d'un fragment se calcule sur sa longueur **avant** rognage. Noter le fragment rogné faisait gagner celui dont la tabulation partait, à égalité de tout le reste |

Les huit derniers ont un point commun avec ce que les cartes précédentes ont
appris : ils ne portent pas sur le découpage, qui était le sujet, mais sur ce
que le champ **contient**, sur la façon dont il est lu, et sur les **bords**.
Le découpage, lui, était juste dès le premier passage — parce qu'il avait été
mesuré caractère par caractère avant d'être écrit.

Deux d'entre eux viennent d'une **plage de contrôle** (900001+), c'est-à-dire
d'une plage jamais utilisée pour corriger. Septième passage, septième plage
neuve qui trouve quelque chose.

Et une leçon d'outillage, la même que la section 2 de `CLAUDE.md` : le premier
lancement de la campagne a signalé un écart de `mapping` et de `field_caps`
**dès la première graine**. Rejoué seul, le cas était vert — c'étaient trois
index laissés par des sondes précédentes sur le conteneur de référence. Un
résultat rouge au démarrage est presque toujours un défaut d'instrument.

### Ce que la brique « n-grammes » a sorti, en un passage

Les n-grammes ont reçu leur brique le jour où ils ont été livrés : un mapping
tiré au sort déclare, une fois sur trois, une section `analysis` avec un
tokenizer `ngram` ou `edge_ngram` **et** un filtre du même genre, aux bornes
tirées au sort, puis un champ `text` peut citer l'un des deux analyzers. Les
deux formes ne sont pas la même : le tokenizer avance d'une position par
gramme, le filtre pose **tous** les grammes d'un mot à la position de ce mot.

C'est cette seconde forme qui a sorti le défaut, sur une plage jamais regardée
(3535000+), deux fois en 250 cas.

| Ce que le fuzzer a sorti | La règle mesurée |
|---|---|
| `match_phrase` sur un champ à n-grammes rendait **3 documents là où ES en rend 12**, en 200 | une phrase n'est pas une suite de **termes**, c'est une suite de **positions**. Tant qu'un analyzer posait un terme par position, la distinction ne se voyait pas ; un filtre à n-grammes en pose dix au même endroit, et Lucene en fait des **alternatives** — une seule position, c'est une union ; plusieurs positions à alternatives, c'est une `MultiPhraseQuery`. ferrite les enchaînait, donc il demandait « ce document contient exactement cette suite de grammes ». La première moitié est faite, la seconde est refusée explicitement (tantivy n'a pas de `MultiPhraseQuery`) |

Deux autres défauts sont sortis de la brique **avant** qu'elle ne pose la
moindre requête, sur les 250 premières graines : un `settings.analysis` posé
dans un **template** était mis en chaîne par la normalisation des réglages, donc
illisible au parseur, et `PUT /{index}/_mapping` lisait son corps avec une
section `analysis` vide. Les deux étaient antérieurs à la carte et invisibles :
ils demandaient qu'un analyzer déclaré rencontre une **autre** fonctionnalité,
ce qui n'arrivait dans aucun test écrit. Une brique nouvelle ne mesure pas
qu'elle-même — elle mesure tout ce qui la traverse.

### Ce que la brique « par requête » a sorti, en un passage

`_delete_by_query` et `_update_by_query` ont reçu leur brique le jour où ils ont
été livrés. Elle est la seule qui **écrit** : elle passe donc en dernier, après
que tout le reste a été comparé, et elle compare deux choses — les compteurs de
la réponse, et **ce qui reste dans l'index**, identifiant par identifiant avec
sa `_version`. Une commande qui rend les bons compteurs en supprimant les
mauvais documents serait verte sur les compteurs seuls.

| Ce que le fuzzer a sorti | La règle mesurée |
|---|---|
| avec `max_docs`, ferrite ne supprimait **pas les mêmes documents** qu'ES | l'ordre de balayage d'un `scroll` sans tri est le `_doc` de Lucene, c'est-à-dire l'ordre d'écriture. Celui de tantivy **n'est pas** l'ordre d'écriture : un `_bulk` de 25 documents en ressort en `d002, d000, d003, d001, …`. La bonne clé était déjà là — le `_seq_no`, attribué sous le verrou d'écriture, qui sert par ailleurs de condition de concurrence. C'était un résultat faux rendu en **200**, sur une commande destructrice |

Et un **prédicat trop étroit de plus**, trouvé par l'autre plage neuve — celui
d'`exists` sur un `text` sans terme, qui ne connaissait qu'un sens :

| Ce qui manquait | Ce que ça a coûté |
|---|---|
| la divergence déclarée sur `exists` était reconnue à un manque **à gauche** | sous un `must_not`, elle rend **plus** de documents à gauche : le document qu'ES juge présent est exclu par ES et gardé par ferrite. Même défaut, signe inversé, et le prédicat le lisait comme un écart réel. Il retourne maintenant le sens quand **tous** les `exists` de la requête sont niés — et seulement après que la sonde a confirmé, en reposant la clause seule aux deux serveurs, que ferrite en voit bien moins. Un prédicat écrit sur un signe doit se demander ce qu'une négation en fait |

Deux garde-fous ont été écrits avec la brique, parce que sans eux elle
mesurerait autre chose que ce qu'elle croit : la même requête est d'abord posée
**en recherche** aux deux serveurs, et le cas est abandonné si elle n'y trouve
pas les mêmes documents (l'écart est alors celui du Query DSL, que l'étape 5
mesure déjà, avec ses prédicats) ; et le **motif** d'un refus n'est pas comparé,
seulement son statut — ferrite nomme ses refus avec ses propres mots, exprès.
Sans le premier, la brique recomptait la divergence `exists` sous un autre nom ;
sans le second, chaque refus légitime des deux serveurs sortait rouge.

### Ce que la brique `fields` a sorti, en un passage

Le générateur ne pose une brique que si `compat.yaml` déclare la capacité
tenue ; les trois paramètres de la carte 18 en ont donc reçu une le jour où ils
ont été livrés. Premier passage sur une plage jamais regardée : **47
divergences**, toutes ramenées à trois règles qu'aucune lecture ne donne.

| Ce que le fuzzer a sorti | La règle mesurée |
|---|---|
| le même champ demandé deux fois rendait le premier format | **la dernière spécification gagne** : `fields: [{"field": "d", "format": "yyyy-MM-dd"}, "d*"]` rend la date au format du mapping, l'ordre inverse au format demandé |
| un champ demandé dans `fields` **et** dans `docvalue_fields` sortait trié | c'est `fields` qui rend la valeur, donc l'ordre du `_source` et ses doublons. Le refus que porte la colonne, lui, reste |
| un `docvalue_fields` sur un `text` refusait même sans document ramené | ces refus sont ceux de la phase de **fetch** : `size: 0` ou zéro correspondance rendent **200** chez ES, qui ne va chercher les valeurs que des documents qu'il ramène |

### Ce que les briques `search_analyzer`, `copy_to` et `store` ont sorti

Trois paramètres de mapping livrés ensemble, trois briques posées le même jour :
un champ `text` qui déclare un analyzer peut en déclarer un second pour la
requête, un ou deux champs se recopient dans une cible (déclarée, ou absente du
mapping pour exercer la création dynamique), et quelques feuilles passent en
`store: true` — que `stored_fields`, déjà tiré au sort, va lire.

Là encore, une brique nouvelle ne mesure pas qu'elle-même. Ce qu'elle a sorti ne
portait sur aucun des trois :

| Ce que le fuzzer a sorti | Ce que c'était |
|---|---|
| une clé de `terms` **entière** sur un champ `float` ou `double` : ferrite rendait `2`, ES rend `2.0` | un défaut **antérieur**, dans la mise en forme des buckets. Le corpus de `diff_aggs.py` n'a pas de valeur flottante entière ; le fuzzer, qui tire `0.0`, `1.0` et `1024.0` exprès, en produit. Un client qui type strictement son JSON y lit un entier là où ES lui donne un flottant. Corrigé |
| un **500** d'ES quand le même champ est demandé par `docvalue_fields` **et** `stored_fields` | un bug d'ES 8.15 (`unsupported_operation_exception`, `reason: null`), pas un défaut de ferrite — qui rend les valeurs, comme il le fait pour chacune des deux lectures prises séparément. Un 500 ne se reproduit pas : c'est déjà la raison pour laquelle `_seq_no` nommé dans `fields` est refusé. Divergence assumée n° 22 |
| l'ordre des valeurs qu'un `copy_to` dépose dans sa cible | ce n'est pas un ordre : c'est l'itération d'un `HashSet<String>` de Java sur {cible} ∪ {sources}. Divergence assumée n° 18, avec un prédicat qui **mesure** que l'écart ne porte que sur l'ordre |
| un mot de plus de **255 caractères** disparaissait de l'index | `maxTokenLength` n'est pas une limite qui jette : les tokenizers de Lucene **coupent** le mot en morceaux de 255 caractères, chacun à la position suivante — donc tout ce qui suit se décale aussi. ferrite jetait le mot entier (et, à 255 pile, un mot que Lucene garde). Défaut **antérieur** : aucun texte du corpus de `diff_analyzers.py` n'avait de mot si long, et il a fallu qu'un `keyword` de 300 caractères soit recopié par `copy_to` dans un `text` pour qu'un tokenizer le voie. Corrigé, et les longueurs 254 / 255 / 256 / 300 / 512 sont entrées dans le corpus |
| `match_phrase_prefix` d'un seul mot sur un champ à n-grammes rendait **un document de plus** qu'ES | `max_expansions` est chez Lucene un budget **par position**, pas par terme : `MultiPhrasePrefixQuery` remplit un seul ensemble en parcourant les termes de la position et s'arrête dès qu'il est plein. La distinction ne se voyait pas tant qu'un analyzer posait un terme par position ; un filtre à n-grammes en pose vingt, et un budget par terme en développe vingt fois plus. Défaut antérieur lui aussi. Corrigé |

Les deux derniers sont la même leçon que celle des n-grammes, une carte plus
tôt : **une brique nouvelle ne mesure pas qu'elle-même**. `copy_to` a fait
entrer une valeur de `keyword` dans un champ `text` — un chemin que rien
n'empruntait — et c'est le tokenizer, pas la copie, qui s'est révélé faux.

Le bug du 500 a coûté un **huitième prédicat trop étroit**, et pour une raison qui
mérite d'être notée : la fonction qui extrait le motif d'une erreur descendait
jusqu'à `failed_shards[].reason.reason` — sauf qu'ici ES rend ce champ à `null`.
L'écart se lisait donc « all shards failed », et aucun prédicat ne pouvait le
distinguer d'un autre 500. Elle rend maintenant le **type** du shard en échec
quand il n'y a pas de phrase. Un instrument qui résume trop finit par effacer ce
qui distinguait deux cas.

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

> Elle **ne sort plus** du passage courant : le tirage a changé, et aucune des
> dix plages ne la repose. Elle reste écrite ici parce qu'une divergence qu'un
> tirage cesse de poser n'est pas une divergence corrigée.

### La divergence ouverte du passage précédent : un ordre que BM25 sépare

`5150000+`, alors plage de contrôle jamais regardée, en avait sorti une seule —
qui n'était **pas** de la famille de la carte d'alors (sa requête ne portait ni
`fields`, ni `docvalue_fields`, ni `stored_fields`).

| Ce qui diffère | Ce que c'est |
|---|---|
| Sur `dis_max: [match_all^2, multi_match "tissu", range]` trié par `_score` puis par un `keyword`, ferrite place `d003` là où ES place `d009` | le `multi_match` note `d003` **2,0223** chez ferrite et **1,8970** chez ES. Le `dis_max` prend le maximum : chez ferrite la valeur passe **au-dessus** du `match_all` boosté à 2,0, chez ES elle reste dessous. C'est l'`avgdl` de BM25 — Lucene le calcule sur les documents **qui ont le champ**, tantivy sur **tous** (le champ `f` n'est rempli que par 20 des 25 documents), et c'est une divergence déjà déclarée dans [`compat.md`](compat.md) |

Elle est **ouverte**, et le prédicat qui couvre cette famille ne l'absorbe
**pas** — volontairement. Il n'accepte une inversion que si ES lui-même donne
deux scores **différents** aux documents échangés ; ici ES les classe ex æquo à
2,0 et c'est ferrite qui les sépare. Élargir la ligne pour l'absorber
masquerait exactement ce qu'elle a été écrite pour attraper : une inversion
causée par une **clé de tri** et non par le score. Le prix, c'est cette ligne
rouge — et elle est le bon prix.

> Elle **ne sort plus** du passage courant, et pour la même raison que la
> précédente : le tirage a changé, aucune des plages ne la repose. Ce
> n'est pas une correction. La cause — l'`avgdl` calculé sur tous les documents
> plutôt que sur ceux qui ont le champ — est toujours là, déclarée dans
> [`compat.md`](compat.md), et un autre tirage la reposera.

Le détail machine est dans [`fuzz.json`](fuzz.json) : les divergences réelles y
sont écrites entières, les assumées résumées par famille avec trois exemples.

### Pourquoi plusieurs plages de graines, et pas une

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
| 8181000+ | les **trois règles de précédence** de `fields` (ci-dessus), 47 divergences en un passage, le jour où la brique a été posée |
| 9494000+ | un **quatrième** prédicat trop étroit : « ES casse sur `epoch_millis` » supposait que ferrite, lui, répondait |
| 5150000+ | la divergence ouverte ci-dessus — un ordre que BM25 sépare, que le prédicat refuse d'absorber |
| 2727000+ | le défaut d'ordre de la commande par requête : `max_docs` ne supprimait pas les mêmes documents qu'ES, en 200 |
| 1414000+ | un **cinquième** prédicat trop étroit : la divergence déclarée sur `exists` change de **signe** sous un `must_not` |
| 3535000+ | le défaut des n-grammes : `match_phrase` rendait 3 documents là où ES en rend 12 |
| 1717000+ | la limite des tokenizers de Lucene : un mot de plus de 255 caractères **coupé** chez ES, **jeté** chez ferrite |
| 2626000+ | rien de neuf — la deuxième plage de contrôle qui n'ajoute rien |

Et à chaque passage suivant, générateur changé, la règle a rejoué exactement
pareil : la plage 1–400 — celle sur laquelle on avait itéré — a sorti le
**troisième** prédicat trop étroit du tableau ci-dessous ; deux plages de
contrôle ont sorti les deux divergences réelles décrites plus haut ; les deux
plages neuves du passage suivant en ont sorti une chacune (un résultat faux
rendu en 200 sur une commande destructrice, et un cinquième prédicat trop
étroit) ; et **les deux plages neuves de ce passage-ci** en ont sorti une
chacune de plus — la limite des tokenizers, et rien du tout pour la seconde,
qui est la deuxième plage muette en dix-sept. Quinze plages, six passages, et
deux seulement se sont tues.

Les prédicats trop étroits, un par passage :

| Ce qui manquait | Ce que ça a coûté |
|---|---|
| le court-circuit d'ES était reconnu à son **type d'erreur** | trois autres refus légitimes de ferrite comptés comme des divergences. Le prédicat regarde maintenant si la requête est court-circuitable, ce qui est la propriété qui explique l'écart |
| `exists` sur un `text` était reconnu à un **compte de documents** | sous un `bool { should: [exists], filter: […] }`, le manque ne se voit que dans le **score** : ES donne 1.0, ferrite 0.0, et aucun compte ne bouge. Le fuzzer repose maintenant la clause `exists` seule aux deux serveurs pour trancher — il le **mesure**, il ne le suppose pas |
| le court-circuit d'ES n'était reconnu que sur le chemin `statut` d'une **recherche** | le même court-circuit se produit à l'ouverture d'un `scroll`, et l'écart s'y lisait comme réel. Un prédicat qui nomme un chemin plutôt qu'une propriété finit toujours par manquer une route |
| « ES casse sur `epoch_millis` » supposait que ferrite, lui, **répondait** | il arrive que les **deux** refusent, pour deux raisons sans rapport : ferrite sur un de ses refus déclarés (un trou entre deux intervalles d'un `range` sur une date), ES sur son bug de formatage — 400 d'un côté, 500 de l'autre, et l'écart se lisait comme réel. Le prédicat porte maintenant sur le **message d'ES** : quand ES n'arrive pas à formater sa propre réponse, il n'y a pas d'oracle et le cas ne mesure rien. Un 500 d'ES pour une autre raison reste un écart |
| « le court-circuit d'ES » ne connaissait que deux déclencheurs **syntaxiques** (`match_none`, `must_not: match_all`) | le troisième ne se lit pas dans la requête : une clause qui ne correspond à **aucun document** vide le `bool` à la réécriture, et ES n'a alors jamais construit les clauses suivantes — donc jamais vu qu'une valeur y était illisible pour le type du champ. Le prédicat le **mesure** maintenant, comme celui d'`exists` : il repose la clause fautive **seule** à ES. Si ES la refuse seule, son 200 sur la requête complète prouve qu'il ne l'a pas construite ; s'il l'accepte seule, ferrite est plus strict qu'ES et l'écart reste réel |
| « refus déclaré » ne demandait pas qu'ES **sache répondre** | c'est pourtant la moitié qui compte, et sa docstring le disait déjà. Le défaut est arrivé par un **progrès** : le texte d'un écart de statut porte désormais les messages des deux serveurs (« statuts 400 / 500 » tout court ne se diagnostique pas), et la phrase de ferrite s'y trouve donc même quand ES échoue de son côté. Un prédicat se relit quand ce qu'il lit change |
| la divergence déclarée sur `exists` était reconnue à un manque **à gauche** | sous un `must_not`, le même défaut rend **plus** de documents à gauche. Un prédicat écrit sur un signe doit se demander ce qu'une négation en fait : il retourne maintenant le sens quand **tous** les `exists` de la requête sont niés, et seulement quand la sonde a confirmé, clause seule, que ferrite en voit moins |
| le **miroir** de la divergence `exists` exigeait qu'ES rende un sous-ensemble de ferrite | vrai sur une page entière, faux dès que la requête tronque : avec `from: 3, size: 2` sur un `must_not exists`, ferrite a un document de plus à paginer, décale tout ce qui suit, et les deux fenêtres sont **disjointes**. Le prédicat d'origine posait déjà cette réserve dans l'autre sens ; son miroir l'avait perdue. Corrigé en la mesurant (`from` non nul, ou page pleine) plutôt qu'en l'assouplissant |
| le motif d'une erreur descendait jusqu'à `failed_shards[].reason.reason` | ES 8.15 rend ce champ à **`null`** quand le même champ est demandé par `docvalue_fields` et `stored_fields` : l'écart se lisait « all shards failed », et aucun prédicat ne pouvait le distinguer d'un autre 500. Le motif rend maintenant le **type** du shard en échec quand il n'y a pas de phrase. Ce n'est pas le prédicat qui était trop étroit, c'est ce qu'il lisait qui était trop résumé |

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
