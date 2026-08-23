# Une vraie application, non modifiée

> L'outil : [`tests/compat/appli_reelle.py`](../tests/compat/appli_reelle.py).
> La mesure du jour : [`docs/application.json`](application.json).

## Le trou que ça bouche

Tout le reste du harnais mesure une **surface d'API** : la suite REST d'Elastic
([`conformance.md`](conformance.md)), le corpus de 5 311 requêtes réelles
([`usage.md`](usage.md)), le fuzzer différentiel ([`fuzz.md`](fuzz.md)). Trois
dénominateurs différents, trois bons chiffres — et aucun qui réponde à la seule
question dont dépend le produit :

> **un logiciel écrit par quelqu'un d'autre, qui n'a jamais entendu parler de
> ferrite, démarre-t-il ?**

Cette page-là répond à celle-ci. Le résultat tient en deux lignes :

> **Gitea v1.27.2, non modifié, passe les 34 cas de sa propre suite
> d'intégration Elasticsearch sur ferrite — les mêmes 34 que contre un vrai
> Elasticsearch 8.15.0.**
>
> **Wagtail v7.1, non modifié, passe les 83 cas de sa suite de backend
> Elasticsearch — et il a fallu trois cartes pour y arriver.**

Chacune a demandé des corrections, et la plupart n'étaient **pas** des manques
de moteur : le refus d'une demande qu'un vrai Elasticsearch sert.

## Ce que la mesure exige pour valoir quelque chose

Trois conditions, tenues par l'outil lui-même — sans elles, « ça marche » est
une impression.

1. **L'application n'est pas modifiée.** Elle est clonée à une révision figée,
   et l'outil relit `git diff` avant de conclure : un fichier suivi qui a bougé
   invalide la mesure. « Ça passe après deux retouches » ne prouve rien, et
   c'est exactement ce qu'on aurait envie de faire.
2. **L'instrument est étalonné.** La même suite tourne d'abord contre un vrai
   Elasticsearch 8.15.0. Une suite rouge contre les deux serveurs ne dit rien
   de ferrite ; une suite verte contre un serveur qu'on n'a pas comparé ne dit
   pas plus. Le rapport publie les **deux** colonnes.
3. **Le trafic est relevé.** Un mouchard s'interpose et journalise chaque
   requête HTTP. C'est lui qui transforme « le cas `Keyword` échoue » en « la
   route, la clause, le message », et c'est la seule façon de savoir ce qu'une
   application envoie **vraiment** — le code source d'un client construit ses
   requêtes, il ne les recopie pas.

Le même prédicat qu'ailleurs dans ce dépôt sert à trier les erreurs : un statut
`>= 400` n'est pas un refus. Le `HEAD /{index}` par lequel Gitea teste
l'existence de son index rend 404 chez les deux serveurs — c'est une réponse.
N'est retenue comme refus que la forme (route, méthode, statut) qu'ES **ne
rend pas** sur la même suite.

## Comment la cible a été choisie

Le critère qui compte n'est pas « laquelle passe », c'est « laquelle est
défendable ». Quatre conditions, posées avant de regarder les résultats :
open source, Elasticsearch employé pour de la **recherche** (pas du log à
l'échelle), une **suite d'intégration à elle** qui parle au serveur, et pas de
dépendance au reste de la stack Elastic — ni sharding, ni sécurité X-Pack, ni
script Painless.

Les candidats ont ensuite été triés sur ce qu'ils **envoient**, lu dans leur
code source et croisé avec [`compat.yaml`](../compat.yaml) :

| Écarté | Pourquoi |
|---|---|
| ReadTheDocs | sa recherche est bâtie sur `highlight` et `inner_hits` (`readthedocs/search/faceted_search.py`) : les deux sont déclarés refusés, et la suite ne mesurerait que l'absence d'extraits |
| Open Food Facts *search-a-licious* | ses index reposent sur des filtres `synonym` que ferrite ne compose pas : l'index ne se créerait pas |
| Zammad, Mastodon | exigent Postgres + Redis + une stack Ruby complète pour lancer un test — le temps passé à monter l'environnement ne mesure rien |
| Graylog, Jaeger, elastalert2 | du log à l'échelle : rotation d'index, ILM, `_msearch`. Ils mesureraient l'exploitation d'un cluster, pas la recherche |

Restent deux cibles, choisies pour être **aux deux bouts** du spectre :

| Retenu | Ce qu'il exerce |
|---|---|
| **Gitea v1.27.2** (MIT) | la recherche d'issues et de PR d'une forge Git. Profil de requêtes identique au sous-corpus `github` de [`usage.md`](usage.md) — `bool`, `multi_match`, `term`, `terms`, `range`, `sort` — et une suite d'intégration pilotée par une variable d'environnement |
| **Wagtail v7.1** (BSD-3) | un CMS Django qui passe par le **client officiel** `elasticsearch-py` 8.x, pose ses propres analyzers et fait tourner sa suite de backend entière contre le serveur |

## Gitea : ce qui a bloqué, et pourquoi ce n'était pas un manque de moteur

Le premier passage s'arrête à la première requête de l'application :

```
create index test_elasticsearch_indexer_….v4: status 400:
  ferrite ne supporte pas le parametre de champ [index] (champ [id])
```

Gitea écrit `"index": true` sur chacun de ses 25 champs. Or `index: true` est le
**défaut** d'Elasticsearch : la valeur ne demande rien de plus que ce que
ferrite fait déjà. Mesuré contre un vrai ES 8.15.0, et c'est la mesure qui
tranche :

```
PUT /s_true  {"properties": {"f": {"type": "keyword", "index": "true"}}}
GET /s_true/_mapping  → {"f": {"type": "keyword"}}          # le parametre a disparu
GET /s_false/_mapping → {"f": {"type": "keyword", "index": false}}   # celui-la est garde
```

Elasticsearch lui-même ne conserve pas `index: true` : il le lit comme
« rien à faire ». Le refuser n'était donc pas un garde-fou contre un échec
silencieux, c'était refuser une demande vide — le même raisonnement que l'objet
**vide** de `script_fields`, accepté pour la même raison.

`index: true` (booléen ou chaîne, les deux écritures d'ES) est donc accepté, et
**`index: false` reste refusé** : ferrite indexerait le champ quand même, et le
client croirait le contraire. Toute autre valeur est refusée comme chez ES
(« seuls `true` et `false` sont admis »). La ligne de
[`compat.md`](compat.md) est passée de ❌ à 🟡, et le scénario
`index_vrai_est_le_defaut` de [`suite.py`](../tests/compat/suite.py) l'exerce
par le client officiel — mapping relu compris.

Avec ce seul changement, l'application **non modifiée** passe.

## Ce que Gitea envoie vraiment

Relevé du mouchard sur un passage complet — 223 requêtes HTTP :

| Route | Requêtes |
|---|---|
| `POST /{index}/_search` | 178 |
| `DELETE /{index}/_doc/{id}` | 28 |
| `POST /_bulk` | 7 |
| `HEAD /{index}` | 5 |
| `GET /` | 2 |
| `PUT /{index}` | 1 |
| `GET /_cluster/health` | 1 |
| `POST /{index}/_doc/{id}` | 1 |

(Le compte des recherches varie d'un passage à l'autre de quelques unités : la
suite attend que l'index se stabilise en l'interrogeant en boucle.)

Les traits exercés, croisés avec le périmètre déclaré : `dsl:bool`,
`dsl:multi_match` (dont `type=phrase_prefix`), `dsl:term`, `dsl:terms`,
`dsl:range`, `corps:sort`, `corps:size`, `type:text`, `type:integer`,
`type:boolean`, `champ:index`. **Aucun trait refusé, aucun indéterminé** — ce
qu'une application de recherche envoie tient dans le périmètre, et c'est
exactement ce que le sous-corpus `github` annonçait (93,2 % servi entièrement).

## La leçon de méthode : ni le corpus, ni la suite d'Elastic ne l'avaient vu

Le correctif qu'il a fallu faire est resté **invisible aux deux autres
mesures** :

| Mesure | Avant | Après |
|---|---|---|
| Corpus d'usage, 5 311 requêtes servies entièrement | 42,1 % | 42,1 % |
| Suite REST d'Elastic, cas en échec | 356 | 356 |

Pas un cas, pas une requête. La raison est structurelle et vaut d'être retenue :
une application ne commence pas par une recherche, elle commence par **créer son
index** — et un corpus fait de corps de requêtes, comme une suite de conformance
écrite pour couvrir des routes, ne pèse presque rien sur ce geste-là. Le premier
appel d'une vraie application est celui que personne ne mesure.

C'est aussi la réponse à « pourquoi cette carte après vingt autres » : les
surfaces d'API étaient couvertes, et il restait un blocage à 100 % pour tout
projet qui pose un mapping écrit par un générateur.

## Wagtail : ce qu'il a fallu pour qu'il démarre

Wagtail v7.1 a été mesuré de la même façon, avec le même outil, et il a servi de
juge à trois cartes de suite. Le résultat du jour tient en une ligne :

> **Wagtail v7.1, non modifié, passe les 83 tests de sa suite de backend
> Elasticsearch sur ferrite — les mêmes 83 que contre un vrai Elasticsearch
> 8.15. Le mouchard ne relève plus un seul refus que ferrite prononce là où ES
> sait répondre.**

| | contre ES 8.15.0 | contre ferrite |
|---|---|---|
| il y a deux cartes | 273 verts | 192 verts, **85 rouges** |
| il y a une carte (n-grammes livrés) | 273 verts | 192 verts, **85 rouges** |
| aujourd'hui | 280 verts | **280 verts, 0 rouge** |
| dont les tests du backend Elasticsearch | **83 verts** | **83 verts** |

Les deux premières lignes disent « 81 tests de backend », celle du jour en dit
83 : ce ne sont pas deux tests de plus, c'est le **parseur** qui en perdait deux
des deux côtés (voir plus bas). Un décompte qui bouge parce qu'on a réparé
l'instrument doit se dire, sinon il se lit comme un progrès.

Les deux premières lignes n'avaient pas bougé d'un test ; ce qui avait bougé,
c'était **où l'application s'arrête**, et c'est ce que cette page publiait à la
place. Le blocage est tombé d'un cran à chaque carte : la section `analysis`,
puis le `PUT /_mapping`, puis rien.

### Les trois paramètres, et pourquoi aucun n'était une demande vide

Après les n-grammes, ce qui séparait Wagtail de ferrite tenait en **trois
paramètres de mapping**. La liste n'était pas déduite du code : le mapping que
Wagtail envoie avait été reposé à ferrite en lui retirant ses paramètres un par
un, et il passait dès qu'on lui retirait ces trois-là.

| Ce que Wagtail demande | Où | Pourquoi il ne pouvait pas être accepté en silence |
|---|---|---|
| `search_analyzer: "standard"` | ses deux champs d'autocomplétion | sans lui, la requête est découpée en grammes elle aussi, donc `elan` rend tout ce qui commence par `e`. C'est le comportement d'ES aussi, mesuré — pas un défaut, mais exactement ce que ce paramètre corrige |
| `copy_to: ["_all_text", "_all_text_boost_2_0"]` | son `_all` reconstitué | l'accepter sans copier rendrait « aucun résultat » sur le champ que **toutes** ses recherches interrogent |
| `store: true` sur `pk` | Wagtail relit ce champ par `stored_fields`, avec `_source: false` | l'accepter sans stocker rendrait un hit sans `pk` — et c'est l'identifiant qu'il utilise pour retrouver l'objet Django. Toutes ses recherches en dépendent |

Les trois sont livrés, mesurés contre ES 8.15 — le détail des bords est dans
[`compat.md`](compat.md#store-copy_to-et-search_analyzer). Ce que Wagtail envoie
vraiment, relevé par le mouchard :

```json
"pk":              {"type": "keyword", "store": true},
"title":           {"type": "text", "copy_to": ["_all_text", "_all_text_boost_2_0"]},
"title_edgengrams":{"type": "text", "analyzer": "edgengram_analyzer",
                    "search_analyzer": "standard"}
```

### Un quatrième refus de trop, caché derrière les trois

Le réflexe a payé une troisième fois. Une fois les trois paramètres servis, la
suite est passée de 0 à 76 cas verts — et les **sept** qui restaient rouges
tenaient à deux causes, dont une seule était un manque :

| Ce qui restait | Ce que c'était |
|---|---|
| **5 cas** — `{"bool": {"mustNot": …}}` | l'écriture camelCase, qu'Elasticsearch 8.15 **sert encore** (`must_not` a gardé cet alias déprécié, et lui seul : `minimumShouldMatch`, `adjustPureNegative`, `maxExpansions`, `caseInsensitive`, `tieBreaker`, `scoreMode` sont tous refusés — mesure, un par un). Un refus de trop de plus, du même genre que l'`index: true` de Gitea, et Wagtail l'écrit sur **chacune** de ses négations |
| **2 cas** — `terms` avec `missing` | un vrai manque : ranger sous une clé les documents qui n'ont pas le champ, ce qu'une facette affiche comme « non renseigné ». Livré, avec ses bords mesurés — tantivy sait le faire, mais pas au bon type sur une date ou un booléen, et pas sans qu'on lui pose la valeur **au type du champ** |

La liste des refus, elle, ne pouvait pas être établie avant : les cinq `mustNot`
étaient derrière le `PUT /_mapping`, comme le préfixe `_` avait été derrière la
section `analysis`. C'est la troisième fois que cette page l'écrit, et c'est
pour ça qu'elle publie « où l'application s'arrête » plutôt qu'un seul chiffre.

### Et un défaut de l'instrument, dans sa forme la plus flatteuse

Le premier rapport « tout vert » comptait **4 cas de moins** du côté d'ES que du
côté de ferrite : ils y sortaient `ABSENT` / `PASS`, ce qui se lit « ferrite
fait mieux qu'un vrai Elasticsearch ». C'était le parseur.

Les quatre cas sont exactement ceux qui envoient `mustNot`. ES y répond avec un
en-tête `Warning: 299 … Deprecated field [mustNot] used, expected [must_not]
instead`, que `elasticsearch-py` transforme en `ElasticsearchWarning` — et que
Python imprime **au milieu** de la ligne `… ok` que le parseur lisait. Le
verdict tombait à la ligne suivante, l'expression régulière ne le voyait plus,
et le cas disparaissait de la colonne du serveur qui **prévient**.

Un résultat massivement rouge est presque toujours un défaut d'outillage ; ce
dépôt l'a payé quatre fois. Celui-ci rappelle l'autre moitié de la règle : un
résultat **flatteur** l'est tout autant, et il ne déclenche aucune alarme. Le
parseur cherche maintenant le verdict jusqu'à l'en-tête du cas suivant, quoi que
le test ait imprimé entre les deux — ce qui a aussi récupéré **sept cas des deux
côtés**, dont deux tests de backend, perdus parce que leur docstring s'intercalait
de la même façon. Le total est passé de 567 à 574, et le compte des tests de
backend de 81 à 83 : un décompte qui monte parce qu'on a réparé l'instrument
n'est pas un progrès, et le dire est le minimum.

### Ce que Wagtail envoie vraiment

Relevé du mouchard sur un passage complet :

| Route | Requêtes | En erreur |
|---|---|---|
| `indices.put_mapping` | 8 589 | 0 |
| `indices.refresh` | 1 547 | 0 |
| `indices.delete` | 1 534 | 18 |
| `indices.create` | 1 534 | 0 |
| `bulk` | 423 | 0 |
| `scroll` | 164 | 0 |
| `search` | 105 | 0 |
| `index` | 105 | 0 |
| `delete`, `count` | 1 + 1 | 0 |

Les 18 `DELETE` en erreur rendent `index_not_found_exception` : Wagtail supprime
avant de créer, et un vrai ES dans le même état rend la même chose — le
mouchard les compte, et le prédicat ne les retient pas comme des refus.

**Wagtail n'a jamais demandé de clause de recherche hors périmètre** : ses
requêtes sont des `bool` + `multi_match` + `term`, comme celles de Gitea. Tout
ce qui l'a séparé de ferrite pendant trois cartes tenait dans la **déclaration**
de son index — la partie qu'aucun corpus de requêtes ne mesure.

## Le classement des échecs

Un échec de suite ne vaut que rangé. Les deux campagnes, mises bout à bout,
donnent quatre catégories — et l'une d'elles n'était pas prévue :

| Catégorie | Ce qui y tombe |
|---|---|
| **Un vrai écart** — ferrite se trompe là où il annonce tenir | **aucun**, sur les deux applications. Aucun cas ne rend un résultat différent d'ES : ce qui échoue, échoue en 400 explicite |
| **Hors périmètre assumé** — déclaré refusé, avec son motif | **aucun** ne bloque plus une des deux suites |
| **Ce qui manque, et vaut une carte** | les **n-grammes**, puis `search_analyzer`, `copy_to` et `store`, puis le `missing` d'une agrégation `terms`. Tous livrés, chacun mesuré contre ES avant de l'être |
| **Un refus qui n'aurait pas dû en être un** | `index: true` ; puis **tout** nom de champ commençant par `_`, et un `PUT /_mapping` qui ignorait les analyzers de son propre index ; puis `{"bool": {"mustNot": …}}`, l'écriture camelCase qu'ES 8.15 sert encore. Ni un manque, ni un choix : le refus d'une demande qu'un vrai ES sert |

La dernière catégorie est celle qui justifie l'exercice, et elle s'est remplie
**trois fois**. Elle ne pouvait pas sortir d'un corpus de requêtes ni d'une suite
de conformance, parce qu'elle ne porte pas sur ce qu'un moteur **sait faire** —
elle porte sur ce qu'un vrai client **écrit**, y compris quand il écrit une
valeur qui ne demande rien, un nom de champ que la règle interdisait d'un cran
trop large, ou le nom déprécié d'un paramètre.

Et elle a une propriété désagréable : **un refus de trop en cache un autre**.
Ceux du second passage étaient derrière le premier, invisibles tant que l'index
ne se créait pas ; celui du troisième était derrière le `PUT /_mapping`. C'est
la raison pour laquelle cette page a publié « où l'application s'arrête » et pas
seulement « combien de tests passent » — pendant deux cartes, le second chiffre
n'a pas bougé alors que le premier reculait à chaque fois.

Une cinquième catégorie s'est ouverte au dernier passage, et elle n'est pas dans
l'application : **un défaut de l'instrument qui flatte**. Quatre cas manquaient
à la colonne d'ES, ce qui se lisait « ferrite fait mieux ». Le décompte publié
ne vaut que si le parseur lit les deux colonnes de la même façon.

## Reproduire

Les deux serveurs, puis l'outil :

```bash
docker run -d --name es-ref -p 9201:9200 \
  -e discovery.type=single-node -e xpack.security.enabled=false \
  docker.elastic.co/elasticsearch/elasticsearch:8.15.0

cargo run --release &                                   # ferrite sur :9200

python3 tests/compat/appli_reelle.py --liste            # les cibles, et les ecartees
python3 tests/compat/appli_reelle.py gitea --json docs/application.json
python3 tests/compat/appli_reelle.py wagtail --json docs/application.json
```

L'application est clonée dans `.appli-reelle/` (ignoré par git) à la révision
figée dans l'outil, et tourne dans l'image Docker de son propre écosystème
(`golang:1.26`, `python:3.12`) — rien n'est installé dans le worker.

Ce qui est épinglé, et pourquoi :

| Épinglé | Raison |
|---|---|
| `gitea` = `1dac1bb2` (v1.27.2) | une mesure sur « la dernière version » n'est pas rejouable |
| `wagtail` = `cf8c53ea` (v7.1) | idem |
| `Django>=5.2,<6`, pour Wagtail | Wagtail 7.1 est antérieur à Django 6 ; sans la borne, `pip` installe Django 6.1 et la suite ne démarre pas. Dérive de dépendances, sans rapport avec ferrite — mais la taire rendrait la recette injouable |

## Ce que cette page ne dit pas

- **Un module, pas toute l'application.** Gitea sait aussi indexer le *code*
  dans Elasticsearch ; ce module-là n'a pas de test d'intégration dans le dépôt
  (seulement un test unitaire hors serveur), donc il n'est pas mesuré ici. Son
  code lit `highlight`, déclaré refusé : il ne passerait pas.
- **34 et 83 cas, pas 34 000.** La suite d'un module d'application est petite
  par nature. Ce qu'elle prouve n'est pas une couverture, c'est qu'un logiciel
  entier se branche : mapping, indexation en masse, recherche, tri, pagination,
  comptage, suppression.
- **Deux applications, pas l'écosystème.** Elles ont été choisies aux deux bouts
  du spectre, avant de regarder les résultats (voir plus haut), mais deux
  logiciels ne sont pas une preuve de généralité. La suivante en trouvera
  d'autres — c'est ce que les trois passages de Wagtail montrent le mieux.
- **Ni la montée en charge ni la durée.** Le prix des mêmes résultats est
  mesuré ailleurs, et il ne se résume pas à un facteur : sur le corpus public
  de la track Rally `geonames`, à deux millions de documents, ferrite est
  devant sur un `term` (×1,8) et derrière d'un facteur 200 sur un tri. Voir
  [`bench.md`](bench.md).
