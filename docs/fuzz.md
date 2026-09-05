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

## Le quatrième verdict : `mort`

Les trois précédents portent sur une **réponse**. Il en manquait un qui porte
sur le serveur : le profil de release de ferrite écrit `panic = "abort"`, donc
une panique atteignable depuis une requête ne rend pas un 500 — elle tue le
processus, et tous les index qu'il servait deviennent injoignables.

Ce fuzzer *voyait* cette mort depuis toujours, et c'est bien le problème : il la
voyait comme un écart de statut de plus (`None` contre `200`), rangé avec les
autres, absorbable par un prédicat de divergence assumée. Une panne générale
déclenchable par un client n'est pas un écart de plus.

Le prédicat `survivant()` est donc explicite, et il tient en une question :

    GET / doit rendre 200

Il est posé **après chaque cas** — une requête par cas, pas une par requête — et
trois choses le distinguent du reste du fichier :

* son verdict (`mort`) n'est absorbable par **aucune** divergence assumée ;
* la campagne **s'arrête** au premier : tout ce qui suivrait mesurerait un
  serveur absent, c'est-à-dire produirait des centaines de faux écarts (la
  cascade que ce dépôt a déjà payée trois fois) ;
* il nomme la **première** requête restée sans réponse, pas la dernière — la
  dernière serait le nettoyage du cas, et le rapport désignerait un `DELETE`
  innocent. Mesuré : c'est exactement ce qu'il faisait avant la correction.

Deux briques de générateur l'alimentent, et elles n'existent que pour lui : elles
posent des entrées que **les deux** moteurs doivent refuser en 400, donc elles ne
peuvent rien apprendre en comparant deux réponses.

| Brique | Ce qu'elle pose |
|---|---|
| `doc.forme` | un objet là où le mapping déclare une feuille (`{"a": {"b": "x"}}` sur un `keyword`), une valeur là où il déclare un objet |
| `date.decalage_illisible` | une borne de date dont le décalage se découpe en octets (`+aéb` : quatre octets, frontière de caractère au milieu) |

Elles font rougir le binaire d'avant, et c'est la seule chose qui prouve qu'elles
mesurent quelque chose (voir « une brique de générateur qui ne fait pas rougir le
binaire d'avant ne mesure rien ») : sur les graines **4200001–4200040**, jamais
utilisées pour corriger, le binaire 0.10.0 meurt dès la première — le rapport
nomme le `_bulk` fautif et le document `{"e": {}}` qu'il transporte. Le binaire
corrigé rend **0 divergence** sur les mêmes graines, et l'étalonnage contre deux
Elasticsearch reste à zéro.

Le fuzzer n'est pas seul sur cette question : les cas déjà trouvés sont figés,
hors d'une graine, dans [`sonde_survie.py`](../tests/compat/sonde_survie.py), et
l'inventaire des points de panique est [`panics.md`](panics.md).

## La mesure du jour

Le générateur a changé — **une brique de plus**, `champ.index_false` : quelques
feuilles du mapping tirées au sort passent en `index: false`, tous types
compris, `text` inclus. Donc **toutes les graines ont changé de sens** : la
campagne précédente ne mesurait plus les mêmes cas, et ses chiffres ne sont pas
reconduits.

La brique ne mesure pas qu'elle-même, et c'est le but : un champ non indexé
reste interrogé par **toutes** les clauses que le générateur tire, parce que
chez Elasticsearch la clause ne disparaît pas — elle change de chemin (la
colonne au lieu de l'index inverse), de score, et parfois de message d'erreur.

Ce tableau est celui de ce passage, sur des plages **jamais utilisées pour
corriger** — la plage `9310000+`, qui a servi à trouver les défauts plus bas,
est publiée à part.

```
plages de contrôle, jamais regardées avant ce tableau
graines 4420000+       120 cas,  5 457 requêtes, 0 divergence
graines 7710000+       150 cas,  6 958 requêtes, 2 divergences (ouvertes, antérieures)
graines 5550000+       150 cas,  6 976 requêtes, 1 divergence  (ouverte, antérieure)
graines 6660000+       250 cas, 11 546 requêtes, 1 divergence  (ouverte, antérieure)
                       ↑ c'est celle-là qui est publiée dans docs/fuzz.json
                     ------------------------------------------------
                       670 cas, 30 937 requêtes, 4 divergences

les mêmes plages, contre le binaire d'AVANT la carte
                       420 cas, 15 688 requêtes, 83 divergences
                                 (24 + 24 + 35)

la plage sur laquelle on a itéré, une fois corrigée
graines 9310000+       120 cas,  5 611 requêtes, 0 divergence
```

Les deux colonnes sont mesurées avec des binaires **release** des deux côtés.

La ligne du milieu dit que la brique mesure quelque chose, et elle se lit avec
la même réserve qu'à chaque carte : `index: false` était **refusé** avant, donc
chaque mapping qui en tire un rendait 400 d'un côté et 200 de l'autre. Ce 83
mesure que la brique est posée souvent, pas qu'elle a trouvé 83 défauts.

Les **trois divergences ouvertes** sont antérieures à cette carte, et c'est
mesuré et non supposé : les trois graines rendent **la même divergence contre le
binaire d'avant**, et deux d'entre elles portent sur un mapping qui ne contient
aucun `index: false`.

| Graine | Ce qui diffère | Ce que c'est |
|---|---|---|
| `7710025`, `7710082` | ferrite refuse en 400 (`field value function must not produce negative scores`), ES répond 200 | un `field_value_factor` à score négatif placé sous un `filter` qui ne retient personne : ferrite prononce le refus, ES ne l'atteint jamais. Sans rapport avec `index: false` — la même divergence sort du binaire d'avant sur la même graine |
| `5550051` | 7 documents contre 5, et une métrique à `-0.1` contre `-0.125` | un `dis_max` de `function_score` sur un mapping **sans aucun champ non indexé**. Identique contre le binaire d'avant |
| `6660176` | même famille que les deux premières (`Missing value for field`) | ferrite prononce le garde-fou du `field_value_factor`, ES ne l'atteint pas. Identique contre le binaire d'avant |

## Le passage de la carte 17 (`collapse`, `post_filter`)

Trois briques de plus — `corps.post_filter`, `corps.collapse` et
`corps.collapse_inner` — et une raison de les séparer : ce qui se mesure sur ces
deux paramètres n'est pas qu'ils filtrent, c'est **où** ils le font. Un
`post_filter` appliqué trop tôt change les agrégations ; un repliement fait après
la pagination rend le mauvais nombre de lignes. Les deux sont silencieux quand
ils sont faux.

```
plages de contrôle, jamais regardées avant ce tableau
graines 17100000+      120 cas,  5 481 requêtes, 9 divergences (ouvertes, antérieures)
graines 17200000+      120 cas,  5 558 requêtes, 3 divergences (ouvertes, antérieures)
graines 17300000+      120 cas,  5 461 requêtes, 6 divergences (ouvertes, antérieures)
graines 17400000+      120 cas,  5 495 requêtes, 2 divergences (ouvertes, antérieures)
                     ------------------------------------------------
                       480 cas, 21 995 requêtes, 20 divergences

les mêmes plages, contre le binaire de `main` (celui d'AVANT la carte)
                       360 cas, 16 624 requêtes, 840 divergences
                                  (308 + 260 + 272)

le générateur d'AVANT, contre les deux binaires — la mesure de non-régression
graines 17100000+      150 cas,  6 903 requêtes, 10 divergences des deux côtés
```

La ligne du milieu dit que les briques mesurent quelque chose, et elle se lit
avec la réserve habituelle : `collapse` et `post_filter` étaient **refusés**
avant, donc chaque requête qui en tire un rendait 400 d'un côté et 200 de
l'autre. Ces 840 mesurent que les briques sortent souvent, pas qu'elles ont
trouvé 840 défauts.

La dernière ligne est celle qui compte pour une régression, et c'est la seule
comparaison honnête : **à générateur constant**, le binaire d'avant et le binaire
d'après rendent exactement les mêmes 10 divergences. Le nouveau code n'en a
introduit aucune.

Les **20 divergences ouvertes** sont antérieures à la carte. Cinq portent une
requête qui contient un `collapse` ou un `post_filter`, et chacune a été
vérifiée à part : `17100078` se reproduit à l'identique **sans** le repliement
(la requête rend 5 documents contre 4 avec ou sans lui — une expression
`query_string`), `17100023` est un `unsupported_operation_exception` qu'ES lève
sur ses **propres** `percentiles`, et les trois autres ont un jumeau sans
repliement dans la même campagne (`17200019` / `17200026`, `17300039`,
`17300086`).

### Ce que ce passage a trouvé : cinq défauts, tous silencieux

Aucun n'était visible aux 97 questions écrites à la main de
[`sonde_repli.py`](../tests/compat/sonde_repli.py) — qui avaient pourtant servi
à **écrire** la fonctionnalité.

| Graine | Ce qui était faux | Ce que c'était |
|---|---|---|
| sept graines | le tableau `sort` d'un hit portait **un élément de plus** | les clés de tri d'un `inner_hits` étaient rangées derrière celles de la racine dans le même tableau, et rendues avec elles. Un `sort` est ce qu'un client relit pour paginer |
| sept graines | `matched_queries` manquait dans les documents repliés | ES les y rend ; ne pas les rendre, c'est taire **pourquoi** ce document-là représente son groupe |
| `17100133` | un `keyword` à valeurs **répétées** faisait tomber la recherche | les `SortedSetDocValues` de Lucene dédoublonnent, ses `SortedNumericDocValues` non : `["x", "x"]` est mono-valué pour ES, `[5, 5]` ne l'est pas |
| quatre graines | `inner_hits` sur un champ **non indexé** répondait | chez ES un `inner_hits` est une vraie sous-recherche, qui a besoin de l'index inverse : il refuse. Le repliement **seul**, lui, y marche — et le réflexe d'étendre le refus au second niveau était faux, ES l'y accepte |
| `17200099` | deux documents ex æquo sortaient dans un autre ordre qu'ES | le départage final était l'adresse tantivy, qui n'est pas l'ordre d'écriture. Défaut **antérieur** à la carte, invisible partout ailleurs parce qu'une liste sans tri à elle n'était jamais rendue au client. Corrigé par le `_seq_no` |

### Ce que ce passage a trouvé : quatre défauts, tous silencieux

Aucun n'était visible aux 244 questions écrites à la main de
[`sonde_index_false.py`](../tests/compat/sonde_index_false.py) — qui avaient
pourtant servi à **écrire** la fonctionnalité, et qui sont la spécification
publiée de `index: false`. Tous les quatre sont dans le **voisinage** du
paramètre, pas dans le paramètre lui-même.

| Graine | Ce qui était faux | Ce que c'était |
|---|---|---|
| `9310061` | ferrite rend **200** là où ES refuse en 400 (`field:[a] was indexed without position data; cannot run PhraseQuery`) | le refus d'une **phrase** sur un `text` non indexé dépend du **nombre de termes**, et la première mesure n'avait posé qu'un seul mot : à un terme il n'y a plus de phrase (c'est un `term`, donc « pas indexé »), à plusieurs c'est la `PhraseQuery` qui manque de positions, à zéro il n'y a pas de clause du tout. Trois réponses pour la même clause |
| `9310045` | aucun fragment surligné là où ES en rend un | la règle « un champ non indexé ne marque rien » était **trop large**. Seule la famille des **automates** marque — `terms`, `prefix`, `wildcard`, `regexp`, `fuzzy` — parce que Lucene les extrait de la requête sans rien demander à l'index ; un `term`, un `match` ou un `range` trouvent le document sans le marquer. `terms` et `term` ne répondent donc pas pareil, à une lettre près |
| `9310029` | aucun fragment là où ES rend celui de `no_match_size` | le repli `no_match_size` s'applique à un champ non indexé comme à un autre : il ne dépend d'aucune correspondance. C'est le même défaut que la ligne au-dessus, vu par l'autre bout |
| `5550060` | ferrite rend **moins** de documents qu'ES sur un `range` posé sur un `boolean` non indexé | chez ES, un `lt` y **efface le reste de l'intervalle** : `{"gt": true, "lt": false}` rend les documents à `false`. Le bord n'existe sur aucun autre type (mesure type par type) et ressemble à un défaut d'Elasticsearch — ferrite le reproduit quand même, les 24 combinaisons de bornes étant mesurées et figées |

Et un cinquième, qui n'est pas un défaut de ferrite mais une **divergence
assumée de plus** : `6660022` a montré qu'`exists` sur un `text` à la fois
`index: false` **et** `store: true` fait rendre un **500** à ES
(`FieldExistsQuery requires that the field indexes doc values, norms or
vectors`) — le champ existe dans son index sans y porter de colonne. Le même
champ **sans** `store` rend 200 et aucun document ; ferrite rend cette
réponse-là dans les deux cas, et un 500 ne se reproduit pas. Le prédicat porte
sur le message d'ES, sinon n'importe quel 500 passerait.

Et un sixième, qui va dans l'autre sens : `9310016` a montré que
`case_insensitive` sur un `regexp` posé sur un champ non indexé est **refusé par
ES lui-même** (`Match flags not yet implemented [256]`), alors que ferrite y
répondait 200. Servir ce qu'un vrai Elasticsearch rejette est le même défaut que
`boost_factor`, dans l'autre sens : il est donc refusé, avec sa phrase.

Les cas correspondants sont figés dans
[`sonde_index_false.py`](../tests/compat/sonde_index_false.py), hors d'une
graine — et le fichier entier rend **147/244 contre le binaire d'avant**, ce qui
est la seule façon de savoir qu'il mesure quelque chose.

## La mesure de la carte suivante (`query_string`, `simple_query_string`)

Deux briques de plus, `q.query_string` et `q.simple_query_string` : elles tirent
une **expression** dans le langage du `QueryParser` de Lucene, bâtie sur le
mapping et la donnée du cas — un champ nommé, une phrase, un préfixe, un joker,
un flou, un boost, une borne — puis, une fois sur quatre, la tordent (opérateur
pendant, guillemet non fermé, `^` sans nombre, borne inachevée). C'est le genre
de code où une entrée tordue trouve un `unwrap`, et le prédicat `survivant` veille
après chaque cas.

La brique fait rougir le binaire d'avant, ce qui est la seule façon de savoir
qu'elle mesure quelque chose : la même plage jouée contre le ferrite 0.11.0 rend
**35 divergences sur 40 cas** (il refuse la clause entière), contre **0** sur le
binaire de cette carte.

```
plages de développement (celles sur lesquelles on a corrigé, donc qui ne mesurent plus rien)
graines 11400000+       60 cas,  2 735 requêtes
graines 11700000+      120 cas,  5 507 requêtes

plage de contrôle, jamais regardée avant ce tableau
graines 12300000+      100 cas,  4 531 requêtes, 0 divergence sur le mini-langage
```

Les trois divergences que la plage de contrôle rend portent sur autre chose et
sont antérieures à la carte : un `function_score` à score négatif, une forme de
document que ferrite accepte et qu'ES refuse, et un `500` d'ES là où ferrite
rend `400` — aucune ne cite `query_string`. Elles sont laissées telles, mesurées.

### Ce que les briques ont trouvé : trois défauts, tous silencieux

| Graine | Ce qui était faux | Ce que c'était |
|---|---|---|
| `11400051` | ferrite refuse (`[prefix] ne s'applique qu'à un champ [text] ou [keyword]`) là où ES rend 200 | sous `lenient`, ES écarte le champ sur **toute** exception, pas seulement sur une valeur illisible. Comme `lenient` vaut `true` par défaut quand la clause ne vise que `*`, un `b:al*` sur une expansion qui contient une date faisait échouer la recherche entière. La frontière que ferrite garde est ailleurs : un refus de **périmètre** n'est jamais avalé |
| `11400029` | ferrite rend **9** documents, ES en rend **5** | deux mots séparés par un blanc font **une** clause chez ES (`split_on_whitespace: false`, figé depuis la 7.0). Invisible sur un champ `text`, décisif ailleurs : sur un `keyword` c'est le terme entier qui est cherché, sur un numérique la chaîne entière est illisible. Le groupe s'arrête devant `AND`, `OR`, `^`, `~` et `:` — cinq frontières mesurées jeton par jeton |
| `11400019` | ferrite rend **0** document, ES en rend **20** | l'arbre de `simple_query_string` est **binaire**, monté de gauche à droite, et un opérateur qui répète celui du sommet l'allonge au lieu de l'emboîter. `a b + c` y vaut `(a OU b) ET c` ; une liste plate en faisait `+a +b +c` |
| `11700020` | ferrite ne rend **aucun** fragment de surlignage là où ES en rend un | le surlignage lit la requête du **Query DSL**, pas la requête tantivy : une clause qu'il ne connaît pas ne marque rien. La clause est donc désormais **traduite** en DSL avant d'être exécutée, ce qui donne aussi `explain` et `matched_queries` sans rien écrire de plus |

Aucun des quatre n'était visible aux 687 questions écrites à la main de
`diff_query_string.py` — les quatre y sont maintenant figés.

## La mesure de la carte suivante (`_name`, `explain`, `_explain`)

Une brique de plus, `corps.nom_de_clause` : elle pose des `_name` au hasard dans
une requête déjà générée, là où ES les accepte (au niveau du champ pour les
clauses qui en citent un, au niveau du corps pour les autres), et la comparaison
porte alors sur le bloc `matched_queries` de chaque hit — **quelles** clauses
sont citées, et **dans quel ordre**.

Le chiffre publié est celui d'**après** la fusion avec les cartes 40 et 41 : le
générateur porte désormais quatre briques (`champ.index_false`, celle-ci,
`q.meta` et `corps.min_score`), et chacune redistribue le tirage. Les plages
mesurées pendant le développement (`9110000+`, `9610000+`) ne désignent donc
plus les mêmes cas et ne sont pas reconduites — c'est la règle du fichier,
appliquée à un rebase plutôt qu'à une carte.

```
plage de contrôle, jamais regardée avant ce tableau, générateur à quatre briques
graines 9710000+       100 cas,  4 559 requêtes, 0 divergence
```

Elle est mesurée avec un binaire **release**, comme les fois précédentes, et
cette précision vient de servir : la même plage jouée contre un binaire **debug**
sort une divergence de plus — un `debug_assert` de tantivy
(`PhraseScorer::seek_danger`, « target (0) should be greater than or equal to
doc (6) ») que le release compile, et dont la réponse est **juste** des deux
côtés. Ce n'est pas un défaut de ferrite : c'est une précondition interne que la
dépendance vérifie plus strictement que son propre contrat. La noter évite de la
rediagnostiquer à la prochaine campagne lancée en debug.

### Ce que la brique a trouvé : deux défauts, tous les deux silencieux

| Graine | Ce qui était faux | Ce que c'était |
|---|---|---|
| `9110050` | ferrite cite le nom d'une clause qu'ES ne cite pas : un `match_all` nommé placé sous un `must_not` | ES **réécrit** un `bool` dont un `must_not` est un `match_all` **nu** en `match_none`, et il le fait au niveau du `QueryBuilder` — donc avant de traduire ses clauses, donc aucun de leurs `_name` n'est enregistré. Pas seulement celui du `match_all` : **tous** ceux du `bool`, y compris son propre `_name`. La règle porte sur la clause exacte — le même `match_all` enveloppé dans un `constant_score` est bien nommé (mesuré). C'est la même réécriture que le surlignage connaissait déjà, sur un autre chemin |
| `9610018` | ferrite rend **200** là où ES rend **400** (`field value function must not produce negative scores`) | une clause nommée est **rejouée, et notée**. Sous un `sort`, la requête principale ne calcule aucun score : c'est donc le `_name` qui rallume le calcul, et le garde-fou du `field_value_factor` tombe. ferrite rejouait bien la clause mais **perdait l'incident** — il le relisait après la recherche, alors qu'il naissait à la restitution. Trois lignes suffisent à l'isoler : sans `sort` les deux serveurs refusent, avec `sort` les deux répondent, avec `sort` **et** `_name` seul ES refusait |

La seconde n'a été trouvée qu'**après le rebase** sur la carte 40, sur une plage
de contrôle rejouée parce que le générateur avait changé. C'est le geste 2
appliqué à un rebase : un cliquet qu'on n'a pas vu passer après une fusion ne
prouve rien, et ici la fusion a bel et bien démasqué un silence. La branche a
été rebasée trois fois au total, et la campagne rejouée à chaque fois — c'est
la seule des trois qui a trouvé quelque chose, ce qui ne rend pas les deux
autres inutiles : on ne sait laquelle paie qu'après l'avoir jouée.

Cinq cas sont figés dans [`sonde_fuzz.py`](../tests/compat/sonde_fuzz.py) : les
deux défauts, et surtout leurs **contreparties**, qui interdisent de corriger
trop large — le même `match_all` sous un `constant_score` *est* nommé, une
clause nommée sous un `must_not` *est* citée quand elle correspond au document,
et un `_name` posé sur une **autre** clause ne rallume **pas** le scoring du
`function_score`.

Et la brique a réveillé un défaut qui n'avait rien à voir avec elle, quatrième
fois de suite : `_validate/query`, `_count`, `_delete_by_query`,
`_update_by_query` et l'`index_filter` de `_field_caps` lisaient la requête sans
en retirer les `_name`, donc rendaient `valid: false` (ou une erreur) sur une
requête qu'ES accepte. Une clause nommée n'est pas seulement lue par `_search`.

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

À côté, une divergence **ouverte** que la campagne de `query_string` a sortie
sans la chercher, et qui n'a rien à voir avec elle : sur un champ `integer`,
`{"term": {"f": "1.0"}}` rend 4 documents chez ES et **400** chez ferrite
(`valeur "1.0" non convertible en un entier`). ES lit la chaîne comme un
flottant puis la tronque si elle est entière ; ferrite refuse la chaîne.
Elle est antérieure à la carte — le binaire 0.11.0 rend la même erreur — et
elle est laissée telle, mesurée : le refus est **bruyant**, ce n'est pas un
silence. Elle sort ici parce qu'une expression `query_string` sur les champs
par défaut pose la même valeur sur tous les types à la fois.

Et une seconde, de la même famille — sortie par la même campagne, antérieure
elle aussi : un `bool` dont une clause **obligatoire** est une négation que le
`_source` ne sait pas trancher (`{"bool": {"must_not": […], "should":
[{"match_all": {}}]}}`) fait perdre au surlignage le fragment d'une **autre**
clause obligatoire du même `bool`, dès qu'un `should` voisin existe. ES le rend,
ferrite non. La règle du dépôt dit pourtant qu'une feuille indécidable est
**supposée satisfaite** — « dans le doute il vaut mieux marquer de trop que se
taire » —, et une négation indécidable ne l'est pas. Elle se reproduit en Query
DSL **écrit à la main**, donc elle n'appartient pas à `query_string` ; c'est le
générateur du mini-langage qui a produit la forme, parce que `-x +y (z)` est une
expression qu'on écrit sans y penser et que personne n'avait posée en JSON. Le
binaire 0.11.0 rend la même chose : elle est laissée telle, et nommée ici.

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
