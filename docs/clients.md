# Les suites des clients officiels, contre ferrite

> Rejouer : `python3 tests/compat/tests_clients.py --liste`, puis
> `python3 tests/compat/tests_clients.py python|go|javascript`.
> Le rapport machine est [`docs/clients.json`](clients.json) — les chiffres de
> cette page en viennent.

## Pourquoi celle-ci, alors qu'il y en a déjà cinq

La page produit annonce depuis le début : **« les clients officiels, sans
modification »**. Cinq mesures du dépôt exercent déjà ce que ferrite répond — le
harnais maison (`run.sh`), les deux suites REST rejouées par *notre* runner
(`conformance_es.py`), le corpus d'usage, le fuzzer, et deux vraies applications
(`appli_reelle.py`). Aucune n'exerce le **client** : toutes passent par un
client, mais c'est nous qui écrivons ce qu'on lui demande.

Ici, ce sont les tests que l'équipe du client a écrits pour son client, joués
par **son** lanceur, dans **son** langage. La différence est exactement celle
qui sépare nos cas de ceux d'Elastic — et elle porte sur une couche que rien
d'autre ne touche : la poignée de main, l'en-tête de produit, la négociation de
compression, le sniffing, la carte statut → exception, les helpers.

Trois conditions, les mêmes que pour les applications réelles :

1. **la suite n'est pas modifiée** — clone à une révision figée, `git diff`
   vérifié après coup, rien de recopié dans ce dépôt (téléchargement à la
   demande dans `.clients-tests/`, comme `.es-rest-spec/`) ;
2. **la licence est vérifiée avant qu'on en dépende** — le fichier `LICENSE` du
   clone doit porter la phrase attendue, sinon l'outil s'arrête. Les trois
   clients sont sous Apache-2.0, constaté dans le clone ;
3. **l'instrument est étalonné** — la même suite tourne d'abord contre un vrai
   Elasticsearch 8.15. C'est elle qui a écarté une des trois suites (voir plus
   bas) et qui retire du dénominateur les cas que le client rate contre le
   serveur d'Elastic lui-même.

## Ce qui a été mesuré

| Client | Révision | Licence | Suite | vrai ES 8.15 | ferrite |
|---|---|---|---|---|---|
| `elasticsearch-py` | v8.15.0 | Apache-2.0 ✔ | `pytest test_elasticsearch/test_server` | 71/84 *(origine)* · 45/84 *(adapté)* | **0/84** *(origine)* · **43/84** *(adapté)* |
| `go-elasticsearch` | v8.13.0 | Apache-2.0 ✔ | `go test -tags integration . ./esapi ./esutil` | 28/30 | 15/30 |
| `elasticsearch-js` | v8.15.0 | Apache-2.0 ✔ | *aucune jouable* — voir « ce qui n'a pas été mesuré » | — | — |

Et, pour les trois, la batterie **cycle de vie du client** — le plancher que la
carte pose. Elle est écrite ici, mais elle est jouée par le client **publié**
(PyPI, npm, le proxy de modules Go) : ce qu'un utilisateur installe.

| Cas | Python | Go | JavaScript |
|---|---|---|---|
| découverte de version | ✅ | ✅ | ✅ |
| en-tête `X-elastic-product` (sur `/`, `_search`, `_bulk`) | ✅ | ✅ | ✅ |
| négociation de compression (`gzip` sur le corps) | ✅ | ✅ | ✅ |
| sniffing, ou son **refus propre** | ✅ refus | ✅ refus | ✅ refus |
| erreurs typées (404 / 400 / 409, `error.type`, `error.reason`) | ✅ | ✅ | ✅ |
| helpers | ✅ `bulk`, `streaming_bulk`, `parallel_bulk`, `scan` | ✅ `esutil.BulkIndexer`, déroulé de `scroll` | ✅ `helpers.bulk`, `scrollSearch` |
| **total** | **9/9** | **7/7** | **7/7** |

Les trois batteries rendent le même score contre un vrai Elasticsearch 8.15. La
seule ligne où les deux serveurs ne font pas la même chose est le **sniffing** :
ES publie ses nœuds sur `GET /_nodes/_all/http`, ferrite refuse cette route en
la nommant — et les trois clients restent utilisables après ce refus, ce que le
cas vérifie explicitement. Un client qui se tairait, ou qui perdrait la main,
serait un échec.

## Ce que ça a trouvé

Trois défauts, tous **silencieux ou muets sur leur cause**, et aucun n'avait été
vu par les 5 311 requêtes du corpus d'usage, par les deux suites REST, par le
fuzzer, ni par les deux applications réelles.

### 1. Un `_id` numérique dans un `_bulk` était un `_id` absent

`{"index": {"_index": "x", "_id": 42}}` — l'écriture que produit `helpers.bulk`
dès que la clé primaire de l'appelant est un entier. ferrite ne lisait que les
chaînes : la métadonnée tombait dans le cas « pas d'identifiant », le document
partait sous un identifiant **tiré au sort**, en `201`, sans un mot. Le `get`
suivant rendait 404.

ES lit **toute valeur simple** et la rend en texte (`42` → `"42"`, `true` →
`"true"`, `null` = absent), et refuse un objet ou un tableau en le nommant.
ferrite fait maintenant les deux, message compris.

Le même piège était **déjà corrigé une fonction plus loin**, sur `_mget`, avec
son commentaire d'explication. Corriger un lecteur ne corrige pas ses voisins.

### 2. Lister *tous* les templates n'est pas les chercher par motif

`GET /_index_template` et `GET /_template` rendaient **404** sur un serveur sans
template, avec le corps juste (`{"index_templates": []}`, `{}`). Un statut faux
sous un corps juste ne se voit pas dans un `curl` — mais tout client qui lève
sur 404 casse, et c'est le premier appel du ménage inter-cas de la suite du
client Python.

ES sépare les deux : sans nom → `200`, même vide ; avec un nom ou un motif qui
ne correspond à rien → `404`. Mesuré sur un ES 8.15 démarré sans ses propres
templates (`stack.templates.enabled=false`,
`xpack.monitoring.templates.enabled=false`) : `GET /_template` y rend `200 {}`
et `GET /_template/*` rend `404 {}`. La même règle est appliquée à
`_index_template`, dont les deux autres branches sont mesurées (`/nope` → 404
nommé, `/nope*` → 404 avec liste vide) mais dont l'inventaire vide n'est pas
observable : un ES 8.15 réinstalle ses templates APM quoi qu'on fasse.

### 3. La compression du corps n'était pas lue du tout

`http_compress=True` (Python), `compression: true` (JavaScript, **activé par
défaut** vers Elastic Cloud), `CompressRequestBody` (Go) : les trois clients
posent `Content-Encoding: gzip` et envoient un corps compressé. ferrite le
lisait comme du JSON et répondait « le corps de `[_bulk]` doit être de l'UTF-8 »
— un refus explicite, donc pas un échec silencieux, mais qui ne nommait pas sa
cause et rendait le client inutilisable dès qu'on activait l'option.

Ce que fait un vrai ES 8.15, mesuré encodage par encodage : `gzip` et `deflate`
(enveloppe zlib) sont décompressés ; `br` et un nom inconnu sont **transmis tels
quels** (Netty n'a pas de décodeur pour eux) ; un flux `deflate` brut, sans
enveloppe, rend `400 request body is required`. ferrite reproduit les trois
premiers cas et, sur le quatrième, refuse **en nommant l'encodage** plutôt qu'en
désignant un corps manquant — la seule divergence, et elle est vers plus
d'information.

Coût : une dépendance (`flate2`, backend `rust_backend` — pas de C à lier, donc
le binaire musl reste statique), **+40 960 octets** sur le binaire et
**+21 331 octets** sur l'image compressée, celle qu'un registre sert. Les
chiffres du conteneur sont republiés dans la même PR, mesurés sur l'image
**reconstruite** — `measure_container.sh` ne construit rien, il mesure ce qui
existe, et la première mesure a republié la taille de la veille sans broncher.

### 4. Une régression que la campagne a laissée ouverte, puis comblée

`?timeout=` sur `_search` était refusé comme un paramètre **inconnu**, alors que
la capacité `recherche.route` est déclarée **tenue** : le rattachement le
comptait donc en **régression**, pas en coût de périmètre. C'était un cas de
`esapi_integration_test.go` du client go, publié tel quel — un chiffre qu'on
subit devient un chiffre qu'on pilote seulement si on ne le range pas soi-même
du bon côté.

Il est comblé, et de la même façon que `preference` : **accepté, vérifié, sans
objet**. Chez ES, `timeout` est une borne *par shard* au-delà de laquelle la
collecte s'arrête et la réponse sort partielle avec `timed_out: true` ; ferrite
cherche en un seul morceau, dans le processus, et n'a rien à interrompre — il
rend toujours un résultat complet et `timed_out: false`, ce qu'ES rend aussi
tant que la borne n'est pas atteinte. C'est le sens sûr : un `timeout` honoré
rendrait *moins* de documents.

Deux choses valent d'être notées, et la seconde est la vraie. D'abord, un
paramètre sans effet dont la valeur n'est pas relue est un nouvel échec
silencieux : `timeout=1` (l'unité manque) rendrait 200 ici et 400 là-bas, et le
client ne découvrirait sa faute qu'en changeant de serveur. La forme est donc
vérifiée, et ses bords viennent d'une mesure et non de la documentation d'ES —
`0` et `-1` **sans unité** sont valides (ils veulent dire « pas de limite »),
`1D` / `1H` / `1MS` passent mais **pas `1M`** (un `M` majuscule voudrait dire
« mois » ailleurs), `-1s` passe et `-2s` non, et un nombre à virgule donne un
troisième message encore (« fractional time values are not supported »).

Ensuite : ce manque n'a été trouvé par **aucune** des deux suites de
conformance. Il l'a été par la suite d'un client, sur trois de ses cas — et il
pèse 94 requêtes du corpus d'usage, dont neuf qui ne passaient qu'à cause de
lui. C'est la troisième source qui paie, exactement comme la seconde paie pour
les trois manques d'alias que seule la suite d'OpenSearch voit (voir
[`conformance.md`](conformance.md)).

## La suite du client go, cas par cas

`go test -tags integration . ./esapi ./esutil` n'a pas de fixture de ménage : la
suite se lance telle quelle des deux côtés. **28/30 contre un vrai
Elasticsearch 8.15, 15/30 contre ferrite**, 13 écarts, tous rattachés :

| Cas | Ce qui le sépare | Verdict |
|---|---|---|
| `TestClientTransport/Persistent` | il lit `total_opened` dans `GET /_nodes/stats/http`, 101 fois, pour vérifier que le transport garde sa connexion | `hors.cluster_distribue` |
| `TestAPI/Search`, `/Headers`, `/OpaqueID` | `_search?timeout=…` | **régression** — voir plus haut |
| `TestTypedClient/Index_&_Search` | `?typed_keys` | `recherche.routing_filter_path` |
| `BulkIndexer/*/External_version` (×3) | métadonnées `version` + `version_type` dans une ligne d'action de `_bulk` | `ingestion.bulk` |
| `BulkIndexer/*/Index_alias` (×3) | métadonnée `require_alias` | `ingestion.bulk` |
| les 3 parents de ces sous-cas | ils échouent parce que leurs sous-cas échouent | — |

Deux choses valent d'être relevées. D'abord, **les trois modes de compression du
`BulkIndexer` passent leurs cas `Default` et `Multiple indices`** — c'est-à-dire
que la moitié de cette suite exerce du `gzip` sur le corps, et qu'avant ce
travail elle rendait zéro de ce côté-là. Ensuite, c'est cette campagne qui a
fait nommer une par une les métadonnées de `_bulk` refusées dans
[`compat.yaml`](../compat.yaml) : elles étaient déclarées « les autres
métadonnées », une phrase que le rattachement ne sait pas lire, donc six écarts
remontaient en **régression** faute d'être écrits.

### Les trois campagnes rejouées, et les deux qui ne bougent pas

Combler `?timeout=` fait passer la suite go de **15/30 à 16/30** contre ferrite,
et surtout la vide de sa seule ligne `regression` : `TestAPI/Search` passe, et
les deux autres cas de la même famille (`Headers`, `OpaqueID`) échouent désormais
sur `_reindex` et sur une forme de sortie, tous deux rattachés à une capacité
déclarée refusée.

Les deux autres clients ont été **rejoués aussi**, et ils ne bougent pas d'un
cas : JavaScript garde son cycle de vie 7/7 des deux côtés et son unique refus
`hors.cluster_distribue`, Python ses **71/84** *(origine, ES)* · **0/84**
*(origine, ferrite)* · **45/84** *(adapté, ES)* · **43/84** *(adapté, ferrite)* et
son cycle 9/9. Le rapport commité n'en porte aucune trace, et c'est voulu : ses
seuls écarts entre les deux campagnes sont des durées d'exécution et un
`_scroll_id`. Ce n'est pas la même chose que de ne pas mesurer — la raison pour
laquelle rien ne pouvait bouger (leur trafic relevé ne contient ni écriture
d'alias ni `timeout` refusé) était une lecture du rapport ; le zéro, lui, est une
mesure.

## Ce que coûte la suite du client Python : deux colonnes, pas une

`test_elasticsearch/test_server` appelle `wipe_cluster` **entre chaque cas**.
Ce n'est pas un test, c'est le ménage — et il passe par dix-huit sondes dont
**seize** demandent des routes qu'un moteur de recherche n'a pas à servir :

```
_rollup/job  _slm/policy  _cluster/state/metadata  _snapshot  _data_stream
_cat/templates?h=name  _component_template  _ilm/policy  _ccr/auto_follow
_tasks  _nodes/shutdown  _ml/calendars  _ml/filters  _ml/datafeeds
_transform  _cluster/pending_tasks  _cat/tasks
```

Résultat, mesuré : la suite **telle qu'elle est écrite** rend **0/84** *(origine)* · **43/84** *(adapté)* contre
ferrite — les erreurs sont toutes levées dans la même fixture, avant que le
moindre test ne commence. Le chiffre est publié tel quel : il dit une chose
vraie, qu'une suite de client suppose un cluster complet. Mais il ne dit rien de
ce que les tests mesurent.

La seconde colonne rejoue la même suite avec un **plugin pytest externe**
([`tests/compat/clients/nettoyage_compatible.py`](../tests/compat/clients/nettoyage_compatible.py),
chargé par `-p`, monté à côté du clone et jamais dedans) qui repose le ménage
sur les seules routes que les deux serveurs servent : suppression des index, des
templates des deux familles, remise à zéro des réglages de cluster. Les fichiers
de test ne bougent pas, et `git diff` le vérifie après coup.

Les deux colonnes sont publiées côte à côte, et les seize routes écartées sont
nommées une par une dans [`docs/clients.json`](clients.json). **Une adaptation
qu'on ne compte pas est une adaptation qui grandit.**

Et l'adaptation **coûte aussi au serveur de référence**, ce qui est la raison
pour laquelle les deux colonnes sont mesurées sous le *même* nettoyage : un vrai
Elasticsearch 8.15 passe **71/84** avec son propre ménage et seulement **45/84**
avec celui-ci. Le nettoyage de remplacement ne retire pas ce que le ménage
d'origine retirait chez lui — pipelines d'ingestion, modèles déployés par les
tests de magasin vectoriel — et des cas héritent de cet état. Comparer les 43 de
ferrite aux 71 d'ES serait donc faux de 26 cas ; c'est 43 contre 45, soit
**5 cas verts chez ES et rouges chez ferrite** :

| Cas | Ce qui le sépare |
|---|---|
| `test_bulk_works_with_bytestring_body` | il compare le bloc `_shards` d'un item de `_bulk` à `{"total": 2, …}` : deux shards, donc un réplica — `hors.cluster_distribue` |
| `test_all_documents_get_moved`, `test_children_are_reindexed_correctly`, `test_reindex_accepts_a_query`, `test_reindex_passes_kwargs_to_scan_and_bulk` | `helpers.reindex` demande `GET /_data_stream/{cible}` **avant** de copier, sur toute cible — `hors.flux_de_donnees`, capacité ajoutée par cette campagne, précisément parce que rien ne l'exerçait avant |

Et **trois cas dans l'autre sens**, ceux qu'il faut lire en premier parce qu'ils
flattent : `test_exists_response`, `test_object_response` et
`test_custom_index_settings_with_collision` sont **rouges chez ES et verts chez
ferrite**. Ce n'est pas une victoire — les trois sont verts chez ES avec son
propre ménage, donc ce que la colonne mesure là, c'est l'adaptation, pas le
serveur.

## Ce qui n'a pas été mesuré, et pourquoi

Le négatif documenté vaut autant que le positif : il dit où la mesure s'arrête.

- **`elastic/elasticsearch-clients-tests`** (Apache-2.0, vérifié). C'est bien la
  source commune des clients récents — mais ce sont les **mêmes cas YAML** que
  `conformance_es.py` rejoue déjà, sur deux sources indépendantes. Les brancher
  ici mesurerait une troisième fois la surface d'API, pas le client.
- **Les tests d'intégration des helpers d'`elasticsearch-js`.** Ils appartiennent
  bien au client — et ils sont **cassés dans son propre dépôt** : ils importent
  `waitCluster` de `test/utils`, qui ne l'exporte pas. Vérifié sur v8.0.0,
  v8.4.0, v8.8.0, v8.11.0 et v8.15.0 ; mesure : **aucun cas vert contre un vrai
  Elasticsearch 8.15 comme contre ferrite** (22 lignes `not ok` des deux côtés).
  C'est l'étalonnage qui l'a montré, et c'est exactement ce à quoi il sert — sans
  lui, ce zéro se serait lu « ferrite ne sert pas les helpers JavaScript ».
  Le client JavaScript n'a donc ici que sa batterie de cycle de vie, jouée par le
  paquet npm publié.
- **`go-elasticsearch` ≥ 8.14.** Ses tests d'intégration démarrent **leur
  propre** Elasticsearch par testcontainers (`internal/testing/e2e`) : ils ne
  peuvent plus viser un autre serveur sans qu'on modifie leur code, ce que la
  première condition interdit. La 8.13.0 est la dernière révision pointable sur
  une URL — c'est une mesure, pas une préférence.
- **`test_rest_api_spec.py` du client Python** et **`make test-api` du client
  go** : les deux rejouent les cas YAML d'Elastic, tirés d'une version d'ES dont
  la licence n'est plus Apache-2.0.

## Détails de protocole

**Le mouchard écoute sur un port imposé.** Le relais qui journalise le trafic se
pose sur le **9200**, pas sur un port libre : quatre cas de la suite du client go
écrivent `localhost:9200` en dur. Un port tiré au sort les ferait viser autre
chose que la cible mesurée — donc ferrite doit écouter ailleurs (`--ferrite`), et
l'outil refuse de partir si 9200 est déjà pris.

**Chaque refus relevé est rattaché à une capacité.** Le journal du mouchard passe
par [`perimetre.py`](../tests/compat/perimetre.py), le même verdict que le
rapport de conformance : `cout_perimetre` (la capacité est déclarée refusée),
`regression` (elle est déclarée tenue), `indetermine` (aucune ne la réclame — et
ça compte **contre** nous). Cette campagne a fait bouger deux choses de la
déclaration elle-même : les métadonnées de `_bulk` refusées sont maintenant
**nommées une par une** au lieu d'un « les autres métadonnées » que le
rattachement ne sait pas lire, et le refus du sniffing est écrit sur
`cluster.nodes`.
