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
**217 textes** français et anglais (`tests/compat/diff_analyzers.py`) : des
phrases, un vocabulaire qui balaie les familles de suffixes, et des mots plus
longs que la limite de 255 caractères des tokenizers de Lucene — qui la
**coupent** au lieu de jeter le mot, et décalent d'autant les positions de tout
ce qui suit.

<!-- table:analyzers -->

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

<!-- table:corps_search -->

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

<!-- table:datemath -->

Une expression malformée est refusée avec **le message d'ES, mot pour mot**
(`unit [q] not supported for date math [-1q]`, `truncated date math [/]`,
`operator not supported for date math [1d]`, `For input string: "…"`). ES les
rend sous un `search_phase_execution_exception` « all shards failed » dont la
`root_cause` porte ce texte ; ferrite rend l'erreur directement, sans cet
empilement.

## Agrégations

Comparées champ par champ à un vrai ES 8.15 sur 73 requêtes
(`tests/compat/diff_aggs.py`), clés de réponse comprises. Ce qui sépare un
`terms` d'une **facette** — `include` / `exclude` et l'ordre par
sous-agrégation — a en plus sa propre sonde,
[`sonde_facettes.py`](../tests/compat/sonde_facettes.py), qui compare le bloc
`terms` entier sur 170 questions.

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
