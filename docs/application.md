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

Cette page-là répond à celle-ci. Le résultat tient en une ligne :

> **Gitea v1.27.2, non modifié, passe les 34 cas de sa propre suite
> d'intégration Elasticsearch sur ferrite — les mêmes 34 que contre un vrai
> Elasticsearch 8.15.0.**

Et il a fallu corriger **une** chose pour y arriver ; elle est décrite plus
bas, et ce n'était pas un manque de moteur.

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

## Wagtail : le résultat négatif, chiffré

Un résultat négatif documenté vaut presque autant qu'un succès — à condition
d'être un **chiffre** et une **liste**, pas une impression. Wagtail v7.1 a donc
été mesuré de la même façon, avec le même outil.

| | contre ES 8.15.0 | contre ferrite |
|---|---|---|
| `wagtail.search`, toute l'app | 273 verts, 0 rouge, 290 ignorés | 192 verts, **85 rouges**, 290 ignorés |
| dont les tests du backend Elasticsearch | **81 verts** | **0 vert** |

Les 192 verts de ferrite sont les tests qui ne touchent pas à Elasticsearch (le
backend base de données de Wagtail). Sur ce que la carte mesure — les tests du
backend Elasticsearch — le score est **0 sur 81**, et cette ligne-là n'a pas
bougé depuis la mesure précédente. **Ce qui a bougé, c'est où il s'arrête**, et
c'est ça que cette section publie.

### Ce qui bloquait, et ne bloque plus

La première mesure s'arrêtait à la **toute première requête** : Wagtail déclare
son analyse dans les réglages de son index, et ferrite refusait la section
entière.

```json
"analysis": {
  "tokenizer": {"ngram_tokenizer":     {"type": "ngram",      "min_gram": 3, "max_gram": 15},
                "edgengram_tokenizer": {"type": "edge_ngram", "min_gram": 2, "max_gram": 15,
                                        "side": "front"}},
  "filter":    {"ngram":     {"type": "ngram",      "min_gram": 3, "max_gram": 15},
                "edgengram": {"type": "edge_ngram", "min_gram": 1, "max_gram": 15}},
  "analyzer":  {"ngram_analyzer": …, "edgengram_analyzer": …}
},
"index": {"max_ngram_diff": 12}
```

C'est l'**autocomplétion** de son admin : chaque titre est découpé en préfixes à
l'indexation. Les n-grammes sont maintenant servis, tokenizer et filtre, et
mesurés identiques à ES sur 210 textes — positions et offsets compris
(`diff_analyzers.py`). Les **87 `PUT /{index}` de la campagne passent** ; le
relevé du mouchard le dit sans commentaire : `indices.create`, 87 requêtes,
**0 en erreur**.

Trois refus de ferrite ont sauté pour y arriver, et **deux d'entre eux
n'étaient pas sur la liste des six** — ils étaient cachés derrière le premier :

| Ce qui bloquait | Pourquoi c'était un refus de trop |
|---|---|
| `settings.analysis.tokenizer`, filtres `ngram` / `edge_ngram`, `index.max_ngram_diff` | le chantier de la carte : trois lignes de la liste des six, mesurées identiques à ES |
| **tout** nom de champ commençant par `_` | Wagtail nomme les siens `_all_text`, `_all_text_boost_2_0`, `_edgengrams`. ES ne réserve que ses **champs de métadonnées** (mesure sur 29 noms : `_score`, `_doc`, `_type`, `_size`, `_all`, `_parent`, `_x` passent tous). ferrite refusait le préfixe entier — la raison était bonne (ses colonnes internes s'appellent `_elem.…`, `_nelem.…`, `_join_parent`), la règle était trop large |
| un champ ajouté par `PUT /_mapping` ne pouvait pas citer un analyzer de l'index | `PUT /{index}/_mapping` lisait son corps avec une section `analysis` **vide**, donc « ferrite ne supporte pas l'analyzer [edgengram_analyzer] » pour un analyzer que l'index venait de déclarer. C'est exactement l'ordre de Wagtail : `PUT /{index}` avec les réglages, puis `PUT /_mapping` avec les champs |

Ces deux derniers sont de la même famille que l'`index: true` de Gitea : ni un
manque, ni un choix — le refus d'une demande qu'un vrai Elasticsearch sert. Et
comme celui de Gitea, aucun des deux n'était visible depuis le corpus d'usage ou
la suite d'Elastic : il faut une application qui **crée son index** pour les
rencontrer.

### Ce qui bloque encore, exactement

Le blocage a reculé d'une requête : il est maintenant sur le `PUT /_mapping`,
et il tient en **trois paramètres de champ**. La liste n'est pas déduite du
code — le mapping que Wagtail envoie a été reposé à ferrite en lui retirant ses
paramètres un par un, et il passe dès qu'on lui retire ces trois-là :

| Ce que Wagtail demande | Où | Ce que ferrite répond |
|---|---|---|
| `search_analyzer: "standard"` | mapping des deux champs d'autocomplétion | « ne supporte pas le parametre de champ [search_analyzer] » — **c'est le blocage** |
| `copy_to: "_all_text"` | mapping — c'est ainsi que Wagtail se refait un `_all` | « ne supporte pas le parametre de champ [copy_to] » |
| `store: true` sur `pk` | mapping ; Wagtail relit ce champ par `stored_fields` | « ne supporte pas le parametre de champ [store] » |

```
PUT /wagtail__wagtailcore_page/_mapping  → 400
  ferrite ne supporte pas le parametre de champ [search_analyzer] (champ [_edgengrams])
```

Les 87 `DELETE /{index}` qui suivent rendent `index_not_found_exception` : c'est
la conséquence (Wagtail supprime avant de créer), pas une cause de plus — un
vrai ES dans le même état rendrait la même chose.

Aucun de ces trois n'est une demande vide, et c'est ce qui les sépare d'`index:
true` : `store: true` demande des champs stockés, `copy_to` une copie à
l'indexation, `search_analyzer` un **second** analyzer pour le côté requête. Les
accepter en silence rendrait des résultats faux — c'est le pire résultat
possible de ce projet, et il n'est pas préférable à un 400.
`search_analyzer` est d'ailleurs celui qui compte le plus ici : sans lui, la
requête d'autocomplétion est découpée en grammes elle aussi, donc `elan` rend
tout ce qui commence par `e`. Mesuré identique des deux côtés — c'est le
comportement d'ES, pas un défaut — mais c'est exactement ce que Wagtail corrige
en posant `search_analyzer: standard`.

**Wagtail ne demande toujours aucune clause de recherche hors périmètre** : ses
requêtes sont des `bool` + `multi_match` + `term`, comme celles de Gitea. Ce qui
le sépare de ferrite tient entièrement dans la **déclaration** de son index, et
la liste est passée de six lignes à trois.

Ce n'est pas un succès, et cette page ne l'appelle pas ainsi : **0 sur 81**
reste 0 sur 81. C'est une feuille de route qui a raccourci, et dont chaque ligne
restante est mesurée plutôt que supposée.

## Le classement des échecs

Un échec de suite ne vaut que rangé. Les deux campagnes, mises bout à bout,
donnent quatre catégories — et l'une d'elles n'était pas prévue :

| Catégorie | Ce qui y tombe |
|---|---|
| **Un vrai écart** — ferrite se trompe là où il annonce tenir | **aucun**, sur les deux applications. Aucun cas ne rend un résultat différent d'ES : ce qui échoue, échoue en 400 explicite |
| **Hors périmètre assumé** — déclaré refusé, avec son motif | les trois paramètres de mapping qui restent à Wagtail (`search_analyzer`, `copy_to`, `store`). Ils causent **85 échecs**, mais une seule cause : le premier |
| **Ce qui manque, et vaut une carte** | les **n-grammes** — le tokenizer et le filtre `ngram` / `edge_ngram`. **Livrés depuis** : la section `analysis` de Wagtail passe, et son index se crée |
| **Un refus qui n'aurait pas dû en être un** | `index: true`, puis — au passage suivant — **tout** nom de champ commençant par `_`, et un `PUT /_mapping` qui ignorait les analyzers de son propre index. Ni un manque, ni un choix : le refus d'une demande qu'un vrai ES sert |

La dernière catégorie est celle qui justifie l'exercice, et elle s'est remplie
**deux fois**. Elle ne pouvait pas sortir d'un corpus de requêtes ni d'une suite
de conformance, parce qu'elle ne porte pas sur ce qu'un moteur **sait faire** —
elle porte sur ce qu'un vrai client **écrit**, y compris quand il écrit une
valeur qui ne demande rien, ou un nom de champ que la règle interdisait d'un cran
trop large.

Et elle a une propriété désagréable : **un refus de trop en cache un autre**.
Les deux trouvés au second passage étaient derrière le premier, invisibles tant
que l'index ne se créait pas. C'est la raison pour laquelle cette page publie
« où l'application s'arrête » et pas seulement « combien de tests passent » :
le second chiffre n'a pas bougé, le premier a reculé de deux requêtes.

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
- **34 cas, pas 34 000.** La suite d'un module d'application est petite par
  nature. Ce qu'elle prouve n'est pas une couverture, c'est qu'un logiciel
  entier se branche : mapping, indexation en masse, recherche, tri, pagination,
  comptage, suppression.
- **Ni la montée en charge ni la durée.** Le prix des mêmes résultats est
  mesuré ailleurs (`bench_vs_es.py`) : ×3,6 en latence, ×6 en indexation.
