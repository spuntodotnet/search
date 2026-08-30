# CLAUDE.md — pour reprendre ferrite dans une session neuve

Ce fichier existe pour une raison précise : la valeur de ce projet n'est pas
dans le code, elle est dans la **méthode** qui l'a produit et dans les
**mesures** qui le tiennent. Reprendre sans ça, c'est refaire les mêmes erreurs
avec plus d'assurance.

Le point d'entrée reste [`docs/dev-workflow.md`](docs/dev-workflow.md) — il dit
quoi lancer, quand faire avancer la carte Notion, comment ouvrir la PR. Ce
fichier-ci dit **comment penser** dans ce dépôt.

---

## Ce qu'est ferrite, en trois lignes

Un moteur de recherche compatible avec l'API Elasticsearch, écrit en Rust
au-dessus de [tantivy](https://github.com/quickwit-oss/tantivy), tenant dans une
image de 4,1 Mo compressés — la taille qu'un registre sert, la seule qu'on
publie, et la définition compte : voir [Le conteneur](README.md#le-conteneur).
Le produit, c'est **« le code client existant ne change pas »**.
L'index inversé n'est pas réécrit : le travail réel est la couche de
compatibilité au-dessus. API annoncée : Elasticsearch **8.15.0**.

## La règle qui prime sur toutes les autres

**La compatibilité se prouve, elle ne se déclare pas.** Une fonctionnalité n'est
finie que quand un vrai client Elasticsearch officiel l'a exercée contre ferrite,
ou qu'un vrai Elasticsearch a répondu la même chose sur la même question.

Corollaire, non négociable : **jamais d'échec silencieux**. Une clause, un
paramètre ou une route non supportés renvoient une erreur explicite au format
d'Elasticsearch. Rendre des résultats faux parce qu'on a ignoré un
`minimum_should_match` est le pire résultat possible de ce projet — pire que de
ne pas supporter la clause du tout.

## La méthode, en dix gestes

Ces dix gestes ont chacun trouvé quelque chose qu'un raisonnement n'aurait pas
trouvé. Ils ne sont pas décoratifs.

### 1. Mesurer contre un vrai Elasticsearch, jamais contre son idée d'Elasticsearch

Un test écrit à la main porte la même idée fausse que le code qu'il teste.
D'où le harnais de `tests/compat/` : mêmes requêtes envoyées aux deux serveurs,
comparaison champ par champ, ordre compris. C'est ce qui a révélé que
l'analyzer `standard` découpait `l'ascension` en deux termes là où Lucene le
garde entier — donc que *tout* texte français était mal indexé.

### 2. Étalonner l'instrument avant de mesurer

Avant de conclure quoi que ce soit sur ferrite, **lancer l'outil contre un vrai
Elasticsearch**. Le runner de conformance échouait sur 419 cas sur 643 à son
premier passage : c'était le runner, pas ferrite. Quatre corrections plus tard,
il passe 992/1173 contre ES — et ses verdicts sur ferrite veulent enfin dire
quelque chose.

Même piège deux fois : le nettoyage entre deux cas échouait (une fois sur un
index en lecture seule côté ES, une fois sur un joker que ferrite refuse), et
**tout cascadait ensuite en « index already exists »**. Un résultat massivement
rouge est presque toujours un défaut d'outillage, pas une découverte.

Même piège une troisième fois, plus discret, en passant de 22 à 107 domaines :
un **template** laissé par un cas s'applique aux index que les suivants créent.
`mget` lisait alors un `_type` là où le cas attendait `null`, et
`indices.stats` refusait d'indexer — aucun de ces échecs ne ressemblait à une
fuite d'état. Un index n'est pas le seul état qu'un cas laisse derrière lui :
templates, pipelines, dépôts, snapshots et réglages de cluster survivent à la
suppression des index. 46 échecs contre un vrai ES, ramenés à 3.

### 3. Séparer « ferrite est incomplet » de « ce n'est pas la même version »

Face à un écart, la question est toujours : **est-ce que ça casserait aussi
contre un vrai Elasticsearch de la version annoncée ?** Le probe 7.x tourne
contre trois serveurs (ferrite, ES 7.10.2, ES 8.15.0) exprès pour trancher ça.
Sur 11 échecs d'un client 7.x, 6 casseraient à l'identique contre un ES 8 : ce
sont des coûts de migration 7→8, pas des manques de ferrite.

Le geste vaut aussi pour la suite de conformance, et il coûte trois minutes :
`conformance_es.py --suites <domaines>` lancé contre le **conteneur de
référence**. Le seul cas de `field_caps` qui échoue encore
(`index_filter` avec une borne `gte: 2019` sur un champ `date`) échoue
**à l'identique** contre un vrai ES 8.15 — la suite est figée à la 7.10, où un
nombre nu se lisait comme une année ; en 8.x c'est un `epoch_millis`, et ferrite
rend la même chose qu'ES. Sans cette mesure, la ligne se lisait « régression ».

### 4. Prendre les tests des autres — et ne pas choisir lesquels

Le harnais maison teste ce à quoi on a pensé. La suite REST d'Elasticsearch
teste ce à quoi *Elastic* a pensé — et c'est elle qui a trouvé les deux vrais
manques (création d'index à l'écriture, routes sans index) qu'aucun test écrit
ici n'avait vus. Voir [`docs/conformance.md`](docs/conformance.md).

**Et une seule suite reste un examen dont on connaît le sujet.** Celle d'Elastic
est figée en 2020 : elle ne peut rien dire de ce qui a été ajouté depuis. D'où
la seconde source, la suite REST d'**OpenSearch** (Apache-2.0, licence vérifiée
dans le dépôt avant usage), jouée par **le même runner**. Les deux rangeaient
chacune 36 échecs en régression et se recoupent sur **12 capacités** — deux
équipes qui butent au même endroit, c'est une mesure ; celle d'OpenSearch en a
sorti **trois** de plus, toutes des routes ou paramètres postérieurs à 2020
(`PUT /{index}/_alias` avec l'alias dans le corps, `must_exist` au retrait d'un
alias, `include_named_queries_score`), qu'un vrai ES 8.15 passe.

Les trois sont comblées, et **c'est la preuve la plus nette que la seconde
source paie** : la suite d'OpenSearch tombe de 36 à 32 régressions (182 → 188
réussites) pendant que celle d'Elastic ne bouge **pas d'un cas** — 354 échecs et
36 régressions avant comme après. Une suite figée en 7.10.2 ne peut pas voir un
paramètre ajouté en 8.13 ; sans la seconde, ces quatre cas n'auraient jamais
figuré dans un dénominateur. Deux règles en sont sorties qu'aucune
documentation ne donne, et elles se contredisent en apparence : `must_exist:
true` se vérifie **par index visé** (un `remove` sur `logs-*` échoue dès qu'un
seul des index couverts ne porte pas l'alias), alors que le 404 **par défaut**
est global — il ne tombe que si toute la requête finit sans rien faire.

Le corollaire coûte une campagne de plus, et il est le vrai contenu du geste :
**« ce n'est pas un défaut de ferrite, les deux moteurs divergent » ne se
décrète pas.** La même suite est jouée contre un vrai Elasticsearch 8.15, et un
cas qu'il échoue lui aussi est rangé `divergence_moteurs` — 93 cas sur 347. Sans
cette mesure, la catégorie serait une opinion dont on choisirait le contenu,
c'est-à-dire un dénominateur qu'on écrit soi-même sous un autre nom. Le rapport
publie aussi les deux comptes qui la rendent lisible : les cas que la référence
n'a pas joués (0), et ceux que ferrite **réussit alors que la référence
échoue** (0) — le sens qui flatte, donc celui qu'on lit en premier.

Corollaire payé cher : **un dénominateur qu'on choisit soi-même ne prouve
rien.** Le runner a porté une liste blanche de 22 domaines sur 107, avec une
bonne raison (« les autres ne mesureraient rien ») et deux conséquences. La
lisible : personne n'est obligé de croire un taux dont on a écrit le
dénominateur. La coûteuse : les alias et `scroll` étaient dans les 85 domaines
écartés, alors que ferrite déclare les tenir — et `indices.get_alias` échouait
sur 10 de ses 31 cas. Le tri d'un cas se calcule sur ce que le serveur répond ;
il ne s'obtient pas en n'envoyant pas la question.

### 5. Élargir le corpus avant de conclure « identique »

Les analyzers passaient 28/28 textes. Le corpus porté à 210 — un vocabulaire qui
balaie les familles de suffixes — a trouvé **5 bugs de plus** dans le stemmer
français, dont deux règles que j'avais écrites de travers. « Identique sur ce
qu'on a testé » n'est pas « identique » : quand un algorithme a des dizaines de
branches, il faut un corpus qui les visite.

### 6. Tirer au sort ce à quoi on n'a pas pensé — après avoir étalonné le tirage

Le harnais teste ce à quoi on a pensé, la suite d'Elastic ce à quoi *Elastic* a
pensé — et elle est figée depuis la 7.10. `tests/compat/fuzz_vs_es.py` tire au
sort un mapping, des documents et des requêtes **dans le périmètre déclaré**, et
compare les deux serveurs. Premier passage : **vingt et un défauts**, tous silencieux,
aucun signalé par un client — le tri d'un champ multivalué (ferrite prenait la
première valeur, ES prend le minimum ou le maximum), l'agrégation `range` qui
inventait un bucket de remplissage, le `range` sur un booléen qui rendait un
**500**, le score d'un `term` sur un numérique qui variait avec le nombre de
valeurs du champ. Voir [`docs/fuzz.md`](docs/fuzz.md).

Deux règles rendent ce chiffre lisible, et elles sont le vrai contenu de la
méthode :

- **une plage de graines sur laquelle on a itéré ne mesure plus rien.** Les
  graines 1–400 sont celles contre lesquelles l'outil a été réglé : leur zéro
  était en partie du surajustement. Le premier passage sur des graines jamais
  regardées en a retrouvé sept, dont un vrai défaut. Il faut donc toujours
  publier une plage **de contrôle**, jamais utilisée pour corriger — et la
  publier à part ;
- **le générateur lit `compat.yaml`, il ne le réécrit pas.** Chaque brique cite
  l'identifiant de la capacité qu'elle exerce ; une capacité renommée casse le
  fuzzer bruyamment, une capacité `refuse` n'est pas émise, et `--couverture`
  imprime les capacités déclarées tenues que le fuzzer **n'exerce pas** ;
- **chaque divergence laissée passer porte un prédicat écrit.** Pas un code
  d'état toléré en bloc : une fonction, avec sa mesure et sa raison. La ligne
  sur l'ordre de pertinence, par exemple, n'accepte une inversion que si ES
  lui-même donne deux scores **différents** aux documents échangés — et c'est
  précisément par les cas où ES les classe ex æquo que le `term` sur un
  numérique est sorti.

### 7. Vérifier une hypothèse sur une dépendance par un spike, pas par une lecture

`tests/spike_nested.rs` mesure deux propriétés de tantivy dont dépend tout le
support de `nested`. Elles ne sont pas documentées comme des garanties : le
spike les verrouille, et cassera bruyamment à la montée de version plutôt que le
jour où on s'appuiera dessus.

### 8. Mesurer la vitesse à la taille où elle compte, et sur le corpus d'un autre

Les chiffres de performance du README étaient mesurés — sur **600 documents et
138 requêtes écrites ici**. Deux défauts, et le second est le vrai : à cette
taille on mesure surtout un aller-retour HTTP, et le corpus comme les requêtes
sont les nôtres, donc le dénominateur aussi. C'est exactement ce que les gestes
2 et 4 interdisent partout ailleurs.

`tests/compat/bench_echelle.py` prend la track Rally `geonames` d'Elastic — son
corpus (11,4 M de documents, taille compressée vérifiée à l'octet près), son
mapping (**lu** dans son `index.json`, pas retapé), ses 31 requêtes — et la pose
aux deux serveurs à 500 000 puis 2 000 000 de documents. Ce que ça change :

- **le résultat n'est plus une victoire.** ferrite gagne sur un `term` (×1,7) et
  sur la mémoire (×8), et perd sur le tri (**jusqu'à ×290**), l'indexation
  (×0,20), le `scroll` (×0,25) et le disque (×0,85). Publier les deux moitiés
  est ce qui rend le reste lisible ;
- **le protocole se mesure aussi, et il a fallu trois campagnes.** Deux défauts
  trouvés après coup, tous les deux flattant ferrite : le `_forcemerge` d'ES
  tournait **avant** ses propres chronomètres (ses fusions le ralentissaient
  pendant sa mesure de latence), et la décompression du corpus était consommée
  **pendant** l'indexation (une constante commune aux deux serveurs, qui
  écrasait l'écart — ES est passé de 38 116 à 58 736 doc/s une fois sortie du
  chronomètre). Deux échelles mesurées avec deux protocoles ne se comparent
  pas : la campagne a été relancée en entier à chaque fois. C'est le geste 2
  appliqué à soi-même ;
- **une explication qui vient à l'esprit n'est pas une mesure.** `match_all`
  change de camp entre les deux échelles ; l'explication évidente — ferrite rend
  toujours un total exact quand ES s'arrête à 10 000 — a été testée et elle est
  **fausse** (`track_total_hits: true` coûte 2,55 ms à ES contre 2,30 ms) ;
- **une brique nouvelle ne mesure pas qu'elle-même**, troisième fois. Le banc a
  trouvé deux défauts qui n'avaient rien à voir avec la vitesse : un `_bulk` de
  plus de 2 Mo refusé en `413 text/plain` alors que `_nodes` annonçait 100 Mo
  (donc la taille de lot par défaut de `helpers.bulk` **et** des tracks Rally),
  et surtout une **sous-agrégation qui perd les documents de ses buckets
  rares** au-delà de 2 048 documents par segment — des valeurs fausses en 200,
  invisibles en dessous de cette taille, donc invisibles à toutes les mesures
  précédentes. Ce dernier a demandé une carte à lui seul, et sa correction est
  la seule fois où ce projet a **épinglé un fork de sa dépendance**
  ([`docs/tantivy-patch.md`](docs/tantivy-patch.md)).

### 9. Brancher un logiciel que personne ici n'a écrit

Les gestes 1 à 8 mesurent des **surfaces d'API** et un prix, avec quatre
dénominateurs différents. Aucun ne répondait à la question dont dépend le
produit : *un logiciel écrit par quelqu'un d'autre démarre-t-il ?*
`tests/compat/appli_reelle.py` clone une vraie application à une révision figée,
**vérifie que rien n'y a bougé**, et lance sa propre suite d'intégration contre
un vrai ES puis contre ferrite. Gitea v1.27.2 y passe ses 34 cas des deux côtés.

Ce qui l'en empêchait est la leçon : `"index": true`, écrit sur chacun de ses
champs par son générateur de mapping, était refusé. C'est le **défaut** d'ES —
qu'ES lui-même ne conserve pas dans le mapping qu'il rend — donc une demande
vide, du même genre que l'objet vide de `script_fields`. Et surtout : **ni le
corpus de 5 311 requêtes ni la suite REST d'Elastic ne l'avaient vu** (42,1 %
avant et après, 356 échecs avant et après). Une application ne commence pas par
une recherche, elle commence par créer son index — et un corpus fait de corps de
requêtes ne pèse presque rien sur ce geste-là. Voir
[`docs/application.md`](docs/application.md).

### 10. Lancer les tests du client, pas seulement passer par le client

Les neuf gestes précédents passent **par** un client officiel — c'est la règle
du projet — mais c'est nous qui écrivons ce qu'on lui demande. Le geste qui
manquait est de lancer **la suite du client, par son propre lanceur, dans son
langage** : `tests/compat/tests_clients.py`. Elle exerce une couche que rien
d'autre ne touche, parce qu'elle est sous le DSL — la poignée de main, l'en-tête
de produit, la négociation de compression, le sniffing, la carte statut →
exception, les helpers.

Ce que ça a trouvé du premier coup, tout silencieux : un `_id` **numérique**
dans un `_bulk` (l'écriture que produit `helpers.bulk` dès que la clé primaire
est un entier) était lu comme un `_id` **absent**, donc un document indexé sous
un identifiant tiré au sort, en 201 ; `GET /_index_template` sans nom rendait
404 avec le bon corps ; et la compression du corps n'était pas lue du tout, donc
un client qui active `http_compress` ne pouvait plus rien écrire.

Trois règles en sortent, et la troisième est la plus coûteuse :

- **une suite de client suppose un cluster complet.** Celle du client Python
  nettoie entre chaque cas par seize routes x-pack que ferrite refuse : telle
  quelle, elle rend 0 cas vert et 82 erreurs, toutes dans la même fixture. Les
  **deux** colonnes sont donc publiées — la suite d'origine, et la même avec un
  nettoyage de remplacement injecté par un plugin externe, les seize routes
  écartées nommées une par une. Une adaptation qu'on ne compte pas est une
  adaptation qui grandit ;
- **une suite peut être cassée chez son auteur.** Les tests d'intégration des
  helpers du client JavaScript importent une fonction que leur propre
  `test/utils` n'exporte pas, depuis la 8.0 : zéro cas vert contre un vrai ES
  **comme** contre ferrite. Sans l'étalonnage, ce zéro se lisait « ferrite ne
  sert pas les helpers JavaScript » ;
- **une suite peut refuser d'être pointée ailleurs.** Depuis la 8.14, le client
  go démarre son propre Elasticsearch par testcontainers : sa suite ne peut plus
  viser un autre serveur sans qu'on modifie son code, ce que la première
  condition interdit. La 8.13.0 est donc la dernière révision mesurable, et
  c'est une mesure, pas une préférence.

Voir [`docs/clients.md`](docs/clients.md).

## Les outils, et ce que chacun répond

Tous depuis la racine. Les diffs exigent Docker (ce sont des outils de
développement, pas de CI).

| Commande | La question à laquelle elle répond |
|---|---|
| `./tests/compat/run.sh` | est-ce que le client officiel 8.x fait tout ce qu'on prétend ? (**112/112**, dont l'export par `helpers.scan`, le date math, la recherche libre, l'expression de noms d'alias, la recherche sans index, `_field_caps`, `_validate/query`, `_stats`, les templates, ce que la réponse transporte — `fields`, `docvalue_fields`, `stored_fields` — la modification par requête, `_delete_by_query` / `_update_by_query`, et les n-grammes de l'autocomplétion, `search_analyzer`, `copy_to` et `store`) |
| `tests/compat/diff_relevance.py` | **les mêmes documents dans le même ordre** qu'ES ? (212/213, 0 écart réel) |
| `tests/compat/diff_against_es.py` | la même *forme* de réponse ? (45/46 ; le seul écart est `_cluster/health`, toujours vert par choix) |
| `tests/compat/diff_aggs.py` | les mêmes agrégations ? (53/53, `filter` comprise, et ce qu'un bucket **vide** doit porter) |
| `tests/compat/diff_analyzers.py` | les mêmes tokens, **aux mêmes positions et aux mêmes offsets** ? (38 batteries × 217 textes : 7 analyzers intégrés, 21 déclarations de n-grammes, les 5 analyzers de Wagtail, et les 5 classes de `token_chars` demandées caractère par caractère — toutes identiques) |
| `tests/compat/diff_datemath.py` | les mêmes documents sur une **borne de date** — `now`, `now-1d/d`, `2026-03-15\|\|+1M`, et l'arrondi selon le côté de la borne ? (276/276, messages d'erreur compris ; 45/276 avant le chantier) |
| `tests/compat/diff_highlight.py` | les mêmes **fragments surlignés** — pas leur nombre, leur contenu exact, balises comprises ? (233 questions, **221 identiques au caractère près, 11 refus assumés, 0 écart** ; `--calibrer` : 233/233 contre deux ES). Le même fichier lancé contre le ferrite d'avant rend **0/233** |
| `tests/compat/diff_motifs.py` | les mêmes documents sur un **motif** — `regexp`, `wildcard`, `prefix`, `match_phrase_prefix` ? (101/101) |
| `tests/compat/diff_multi_index.py` | `index=["a","b"]`, `logs-*`, les alias : **les mêmes index visés, fusionnés pareil** ? (87/87, 0 écart, plus aucune divergence assumée ; `--calibrer` : 87/87 contre deux ES) |
| `tests/compat/sonde_msm.py` | les mêmes documents sur un **`minimum_should_match`** — entier, pourcentage, formes négatives, conditions `3<90%`, et sous un `nested` ? (53/53) |
| `tests/compat/releve_mots_vides.py` | quelle est **vraiment** la liste de mots vides d'un analyzer d'ES ? |
| `tests/compat/sonde_fields.py` | **ce que la réponse transporte** — `fields`, `docvalue_fields`, `stored_fields`. Compare le **hit entier** (bloc `fields` clé par clé, présence de `_source`, présence de `_id`) : 103/110 identiques, 3 refus assumés écrits, 4 différences d'ordre assumées, 0 écart. Refuse de tourner si elle ne trouve pas les deux serveurs |
| `tests/compat/sonde_par_requete.py` | **modifier ou purger par requête** — `_delete_by_query`, `_update_by_query`. Compare les compteurs de la réponse **et l'état laissé derrière** (documents restants, `_version`, `_source`) : 62/74 identiques, 12 refus assumés écrits, 0 écart. Les conflits sont provoqués pour de vrai, par une écriture non rafraîchie. Refuse de tourner sans ses deux cibles |
| `tests/compat/sonde_alias.py` | les mêmes alias sur une **expression de noms** — liste, joker, exclusion, `_all` — et le même 404 ? (21/21, corps et message compris) |
| `tests/compat/sonde_ecriture_alias.py` | et pour **écrire** un alias ? Les sept URL de `put_alias` (le nom de l'alias, celui de l'index, ou les deux, viennent du corps — qui **remplace** le chemin), `must_exist`, et les deux règles de 404 qui ne sont pas la même : `must_exist: true` se vérifie **par index visé**, le 404 par défaut est **global**. Compare le statut, le message **et l'état laissé derrière** : **57/65 identiques, 7 refus assumés, 1 message non comparé, 0 écart** — et **14/65 contre le ferrite d'avant**. `--calibrer` : 64/65 contre deux ES |
| `tests/compat/sonde_vide.py` | sur un serveur **sans aucun index**, la même chose qu'ES — et rien accepté en silence ? (28/28 identiques, 0 refus muet ; les deux serveurs doivent être vides, c'est l'état mesuré) |
| `tests/compat/fuzz_vs_es.py` | et **en dehors** des combinaisons auxquelles on a pensé ? Mapping, documents et requêtes tirés au sort dans le périmètre déclaré (`compat.yaml` dit ce qui est jouable), posés aux deux serveurs. **3 500 cas, 162 122 requêtes, 3 divergences ouvertes** (un `avg` sur des `long` aux extrêmes de l'`i64`, un surlignage sous `nested`, et un ordre que BM25 sépare et qu'ES rend ex æquo), sur quatorze plages de graines dont **aucune** n'a servi à corriger — celle sur laquelle on itère ne mesure plus rien, et le générateur ayant changé, les plages du passage précédent ne mesurent plus les mêmes cas. 21 défauts silencieux trouvés au premier passage, 27 de plus depuis — dont **dix-sept** sur le seul surlignage, tous invisibles aux 233 questions écrites à la main. S'étalonne contre **deux** Elasticsearch avant de servir : `--calibrer` (60 cas, 2 664 requêtes, 0 divergence réelle) |
| `tests/compat/sonde_fuzz.py` | les écarts trouvés par le fuzzing, **figés** hors d'une graine (80/80, plus 12 refus assumés) |
| `tests/compat/appli_reelle.py` | **un logiciel écrit par d'autres démarre-t-il ?** Clone une vraie application à une révision figée, vérifie que rien n'y a bougé, lance sa **propre** suite d'intégration contre un vrai ES puis contre ferrite, et relève tout le trafic HTTP au passage. Gitea v1.27.2 : **34/34 des deux côtés**. Wagtail v7.1 : **83/83 des deux côtés**, et plus un seul refus que ferrite prononce là où ES répond. Voir [`docs/application.md`](docs/application.md) |
| `tests/compat/tests_clients.py` | **la suite de tests du client officiel passe-t-elle ?** Pas « un client se connecte » : les cas que l'équipe du client a écrits, joués par **son** lanceur, dans **son** langage. Trois clients, licence Apache-2.0 vérifiée **dans le clone**, révision figée, arbre vérifié intact. `go-elasticsearch` v8.13.0 : 28/30 contre un vrai ES, 15/30 contre ferrite, chaque écart rattaché à une capacité. `elasticsearch-py` v8.15.0 : 71/84 *(origine)* · 45/84 *(adapté)* / 43/84 avec le nettoyage de remplacement, **0/84 telle quelle** — sa fixture nettoie par seize routes x-pack, et les deux chiffres sont publiés. Et le **cycle de vie du client**, joué par le client publié : 9/9 en Python, 7/7 en Go, 7/7 en JavaScript, des deux côtés. Voir [`docs/clients.md`](docs/clients.md) |
| `tests/compat/genere_compat.py` | le périmètre déclaré et la doc disent-ils la **même chose** ? [`compat.yaml`](compat.yaml) est la source (une entrée par capacité : état, paramètres, motif du refus, poids d'usage) ; [`docs/compat.md`](docs/compat.md) et [`docs/compat.json`](docs/compat.json) en sont **générés**, et la CI échoue s'ils divergent |
| `tests/compat/perimetre.py` | ce cas qui échoue, il porte sur quoi ? Il rattache un échec de conformance à une capacité déclarée : **régression** si elle est annoncée supportée, **coût de périmètre** si elle est annoncée refusée |
| `tests/compat/recolte_usage.py` | à quoi ressemblent les requêtes que les gens envoient **vraiment** ? Constitue le corpus ([`tests/compat/usage/corpus.jsonl`](tests/compat/usage/corpus.jsonl), 5 311 requêtes) depuis quatre sources citables : doc de référence 8.15, tracks Rally, clients officiels, code open source. Chaque requête porte l'URL d'où elle vient |
| `tests/compat/ponderation.py` | **quelle part de ces requêtes passe entièrement ?** (42,9 % du corpus, mais **96,2 % du code d'application** et 28,6 % des tracks Rally — l'écart *est* le résultat). Écrit les `poids` de `compat.yaml`, publie [`docs/usage.json`](docs/usage.json) et la table « ce qui manque, par fréquence d'usage ». `--rejoue` pose la même requête à ferrite et à un vrai ES 8.15 : les deux mesures s'accordent sur 99,3 % des cas |
| `tests/compat/conformance_es.py` | que disent les suites de tests **d'Elastic** et d'**OpenSearch** ? Deux sources indépendantes (`--source`), Apache-2.0 toutes les deux, **107** et **112 domaines**, sans liste blanche. Leurs rapports sont des fichiers, pas des phrases : [`conformance.json`](docs/conformance.json) et [`conformance-opensearch.json`](docs/conformance-opensearch.json) (totaux, trois taux, exclusions comptées, détail par cas), régénérés par `--json`, tenus par un cliquet en CI (`--diff`). `--divergences` range à part les cas qu'un **vrai ES 8.15 échoue lui aussi** sur la même suite — mesuré ([`conformance-opensearch-es8150.json`](docs/conformance-opensearch-es8150.json)), pas décidé. `--etat` vérifie entre deux cas que rien n'est **apparu** depuis l'état de départ de la cible — index, alias, templates, réglages de cluster — et arrête la campagne au premier écart (+27 %, payés par la CI) : 79 campagnes consécutives rendent le même rapport à l'octet près |
| `tests/compat/bench_vs_es.py` | mêmes résultats, **et à quel prix** ? Garde-fou de développement : 600 documents et 138 requêtes **écrites ici**, donc un dénominateur qu'on a choisi soi-même — ne sert plus à publier |
| `tests/compat/bench_echelle.py` | et **à l'échelle**, sur un corpus que nous n'avons pas écrit ? La track Rally `geonames` d'Elastic (Apache-2.0, révision figée, corpus vérifié à l'octet près), 500 000 et 2 000 000 de documents, **ses** 31 requêtes. `term` ×1,7 et `match_phrase` ×2,6 pour ferrite a deux millions de documents (et l'avance **grandit** avec la taille), RSS ×8 en sa faveur — et le **tri jusqu'a ×290 contre lui**, l'indexation ×0,20, le `scroll` ×0,25. 13 requêtes jouables, 18 refusées, toutes rattachées à une capacité déclarée. Voir [`docs/bench.md`](docs/bench.md) |
| `tests/compat/sonde_sous_aggs.py` | une **sous-agrégation** voit-elle tous les documents de son bucket ? (46 combinaisons parent × sous-agrégation, 50 000 documents déséquilibrés : **46/46** avec l'épingle de tantivy, **32/46** sans ; `--seuil` rejoue les deux bornes du défaut, 2 047 juste / 2 048 faux) |
| `tests/compat/verifie_tantivy.py` | **qu'est-ce que l'épingle de `Cargo.toml` contient ?** Télécharge les 9 crates publiées que le `[patch.crates-io]` remplace et les compare fichier par fichier à l'arbre du fork : 0.26.1 à l'octet près, plus exactement un fichier. Tourne en CI. Voir [`docs/tantivy-patch.md`](docs/tantivy-patch.md) |
| `./tests/compat/measure_container.sh --json docs/container.json` | **que pèse le conteneur, et sous quelle définition ?** Mesure les deux images que le README compare — ferrite et l'ES de référence — dans la même campagne, avec le même outil, et écrit [`docs/container.json`](docs/container.json) : une entrée par image, et pour chaque valeur sa **définition en une phrase**. `chiffres_conteneur.py --injecte` publie ces chiffres dans le README et [`docs/bench.md`](docs/bench.md), `--verifie` est le cliquet de la CI — un chiffre de conteneur ne se saisit plus à la main |
| `tests/compat/probe_es7.py` | un **client** 7.x peut-il se brancher ? |
| `tests/compat/diff_es7.py` | une **instance** 7.x peut-elle être reprise ? `--inventaire` liste ses types de champ |

Un chiffre qui bouge dans ce tableau se met à jour **dans la PR qui le fait
bouger**, pas après.

## Les décisions déjà prises (ne pas les rejouer sans raison)

- **Le périmètre déclaré est une donnée, pas une prose.** `docs/compat.md`
  était une table tenue à la main de 756 lignes : excellente, et dérivante — la
  page de présentation annonçait encore « scroll : pas encore » des mois après
  sa livraison, parce qu'une table écrite à la main ne peut pas être la source
  de vérité de trois endroits. La source est maintenant
  [`compat.yaml`](compat.yaml) ; la doc et sa forme machine en sont générées, et
  le rapport de conformance **croise** chaque cas échoué avec elle. C'est ce qui
  transforme « 354 échecs » en « 36 régressions et 318 coûts de périmètre »
  (la mesure du jour, dans [`docs/conformance.json`](docs/conformance.json)) :
  la différence entre un chiffre qu'on subit et un chiffre qu'on pilote. Le
  garde-fou est le troisième verdict : un cas qu'aucune capacité ne réclame
  compte **contre** nous, sinon oublier de déclarer une capacité ferait monter
  le taux.
- **Un fragment de surlignage se reproduit, il ne s'invente pas.** Le bloc
  `highlight` aurait pu se livrer en une journée : marquer les termes trouvés
  et couper autour. Ça aurait rendu, sur presque chaque texte, **d'autres
  fragments** qu'Elasticsearch — sans que rien ne le signale, puisqu'un
  fragment plausible ressemble à un fragment juste. Ce que ferrite reproduit
  est donc le `UnifiedHighlighter` de Lucene tel qu'ES le configure, mesuré
  pièce par pièce ([`src/highlight.rs`](src/highlight.rs),
  [`src/segments.rs`](src/segments.rs)) : la fusion des phrases jusqu'à
  `fragment_size` puis la re-coupe au mot, les frontières de phrase d'UAX#29 —
  dont la règle SB8, qui fait qu'un point suivi d'une **minuscule** ne termine
  rien —, les frontières de mot **du JDK** (qui ne sont pas celles de la norme :
  le tiret y joint, le deux-points non), le `PassageScorer` pour choisir
  lesquels survivent. Trois conséquences de méthode : le découpeur a été
  **mesuré avant d'être écrit** (quatre sondes contre le conteneur de
  référence) et il n'a plus bougé ensuite ; ce qui n'est pas reproduit est
  refusé en le nommant (`type`, `order: score`, `encoder`…) plutôt qu'ignoré ;
  et les seize défauts que le fuzzer a sortis portaient tous sur ce que le
  champ **contient** ou sur les **bords**, jamais sur la coupe.
- **Le surlignage garde la forme booléenne de la requête.** ES ne marque pas
  « les termes de la requête » : il marque ce qui a fait correspondre **ce
  document-là**, via les `Matches` de Lucene. Un `should` placé sous un
  `filter` qui échoue ne marque rien. Une extraction à plat était plus simple
  et fausse en 200 ; l'arbre de clauses est donc conservé et évalué document
  par document — et les feuilles qu'on ne sait pas trancher depuis le `_source`
  (un intervalle de dates, une jointure) sont **supposées satisfaites**, parce
  que dans le doute il vaut mieux marquer de trop que se taire.

  Le pendant de cette décision est un **refus** : `require_field_match: false`
  fait chercher chez ES les termes de toutes les clauses dans tous les champs,
  par une extraction dont il documente lui-même le résultat comme approximatif
  — et dont trois passages de fuzzing n'ont pas réussi à retrouver tous les
  cas (l'automate d'un `range` y quitte son champ **et** son type). Reproduire
  un mode *presque* juste, c'est rendre des fragments silencieusement
  différents ; il est donc refusé, en le nommant. Quatre requêtes du corpus
  d'usage sur les 102 qui citent `highlight` le posent.
- **Un objet n'est pas un champ.** `object` est indexé par chemins pointés
  (`client.ville`), exactement comme le fait Elasticsearch. C'est ce qui a rendu
  le chantier petit : `Fields.mapped` était déjà une table `chemin → champ`.
- **`nested` sans jointure de bloc.** tantivy n'a pas l'équivalent du
  `ToParentBlockJoinQuery` de Lucene, mais il conserve l'ordre des valeurs d'un
  champ multivalué (mesuré). Chaque champ sous un `nested` a donc une colonne
  jumelle qui dit de quel élément vient chaque valeur. Raisonnement complet et
  chemin alternatif : [`docs/nested-join.md`](docs/nested-join.md).
- **`join` est plus simple ici que chez Elastic.** Mono-shard, parent et enfant
  sont forcément au même endroit : pas de *global ordinals*, pas de `routing`
  obligatoire. Deux passes suffisent.
- **Les stemmers de Lucene sont portés** dans `src/stemmer.rs`, parce que celui
  de tantivy (Snowball) n'est celui d'aucun des deux : `french` et `english`
  sont mesurés identiques à ES sur 210 textes. Un analyzer n'est **jamais**
  livré sous le nom d'ES tant qu'il n'est pas mesuré identique. Les analyzers
  **sur mesure**
  (`settings.analysis`), eux, sont supportés : ils se composent de briques que
  ferrite reproduit à l'identique (`standard`, `lowercase`, `asciifolding`,
  `stop`, `ngram`, `edge_ngram`).
- **Une phrase est une suite de positions, pas une suite de termes**
  ([`src/dsl.rs`](src/dsl.rs)). Tant qu'un analyzer posait un terme par
  position, la distinction ne se voyait pas ; un filtre à n-grammes pose **tous
  les grammes d'un mot au même endroit**, et là elle décide de tout. Lucene en
  fait des **alternatives** : une seule position, c'est une union ; plusieurs
  positions à alternatives, c'est une `MultiPhraseQuery`. ferrite fait la
  première et refuse explicitement la seconde, tantivy n'ayant pas
  d'équivalent. Les enchaîner — ce qu'il faisait — rendait « ce document
  contient exactement cette suite de grammes », donc beaucoup moins de
  documents, en 200.
- **`token_chars` nomme des catégories générales d'Unicode**
  ([`src/ngram.rs`](src/ngram.rs)), lues chez Lucene par `Character.getType`.
  Les prédicats de la bibliothèque standard de Rust n'y correspondent pas :
  `is_alphabetic` accepte `Ⅰ` (Nl) et les signes vocaliques indiens que
  `isLetter` refuse, `is_numeric` accepte `½` et `①` (No) que `isDigit` refuse,
  et `is_whitespace` accepte l'espace insécable que `isWhitespace` refuse. D'où
  les tables générées de [`src/unicode_classes.rs`](src/unicode_classes.rs), et
  la mesure caractère par caractère contre ES dans `diff_analyzers.py`.
- **Le préfixe `_` n'est pas réservé, les champs de métadonnées le sont.** ES
  ne refuse que ses propres champs (`_id`, `_index`, `_source`, `_routing`,
  `_field_names`, `_ignored`, `_seq_no`, `_version`, `_nested_path`,
  `_feature`, `_data_stream_timestamp`, `_tier`) — `_score`, `_doc`, `_type`,
  `_size`, `_all`, `_parent` et `_all_text` passent. ferrite y ajoute les
  racines de ses **colonnes internes** (`_elem`, `_nelem`, `_join_parent`),
  refusées avec leur raison écrite.
- **La syntaxe de `regexp` est traduite, jamais transmise telle quelle**
  ([`src/regexp.rs`](src/regexp.rs)). Celle de Lucene et celle du crate `regex`
  se ressemblent assez pour qu'on croie pouvoir passer le motif directement, et
  divergent là où personne ne regarde : `^` et `$` ne sont pas des ancres, `@`
  veut dire « n'importe quelle chaîne », `\w` s'arrête à l'ASCII,
  `case_insensitive` ne replie que les caractères isolés (`[d-e]` ne matche pas
  `D`). Aucune de ces règles n'était devinable — `\d` valait encore la lettre
  `d` chez Lucene 8 — et toutes viennent d'une mesure contre un vrai ES 8.15.
  Les quatre opérateurs qu'un automate de `regex` ne sait pas construire (`~`,
  `&`, `<n-m>`, `#`) sont refusés explicitement, jamais pris pour des littéraux.
- **Une expression d'index se résout à un seul endroit** ([`src/selection.rs`](src/selection.rs)) :
  `a,b`, `logs-*`, `_all`, `-exclusion`, alias. Toutes les routes passent par
  là, donc un motif veut dire la même chose partout. Le multi-index s'exécute
  index par index puis fusionne — c'est le `query_then_fetch` d'ES appliqué à
  des index mono-shard, et les agrégations se fusionnent sur leurs résultats
  **intermédiaires** (sinon un `avg` fusionné serait la moyenne des moyennes,
  donc faux).
- **`action.destructive_requires_name` vaut `true`**, comme ES depuis la 8.0 :
  `DELETE /logs-*` est refusé tant que le réglage n'a pas été basculé via
  `PUT /_cluster/settings`. Obéir là où ES refuse ferait de la première
  différence de comportement entre les deux serveurs une suppression de
  données.
- **Un champ inconnu dans une requête ne correspond à rien, comme chez ES** —
  c'est `index.query.parse.allow_unmapped_fields`, le vrai réglage d'ES, avec
  son défaut (`true`). Ça a longtemps été l'inverse, et la décision était
  défendable : sans mapping dynamique, un champ inconnu ressemble toujours à une
  faute de frappe. Un vrai client l'a démentie — un filtre `archiveAt` posé sur
  chaque recherche, jamais mappé faute de commande archivée, faisait échouer
  l'application entière en 400. Le mode strict reste disponible index par index
  (`allow_unmapped_fields: false`). Un sous-champ de `nested` interrogé depuis la
  racine, lui, reste une erreur. Toutes les divergences assumées sont listées et
  justifiées dans [`docs/compat.md`](docs/compat.md).
- **`scroll` fige un `Searcher`, il ne rejoue pas la requête.** Un contexte
  balaie tout le résultat à l'ouverture et garde l'instantané tantivy du moment :
  chaque document sort une fois et une seule, la Nième page ne coûte pas N
  recherches, et ce qui est écrit pendant l'export ne s'y invite pas. Le prix est
  la mémoire du contexte, d'où le `keep_alive` et la purge.
- **Une borne de date est une expression, et elle s'arrondit par son côté.**
  `{"lt": "now"}` se résout côté serveur ; `{"lte": "2026-03-15"}` couvre la
  journée entière alors que `{"lt": "2026-03-15"}` s'arrête à minuit. Les deux
  moitiés viennent du même endroit ([`src/datemath.rs`](src/datemath.rs)) parce
  qu'elles sont le même geste : résoudre une borne **en sachant de quel côté
  elle est**. La seconde moitié n'était dans aucune demande — c'est la mesure
  contre ES qui l'a trouvée, et c'est elle qui rendait des résultats faux en
  silence, là où `now` échouait au moins bruyamment.
- **L'agrégation `filter` est exécutée par ferrite.** Celle de tantivy prend une
  chaîne dans sa propre syntaxe de requête — inutilisable. Mais son sens est une
  intersection de requêtes, et le Query DSL de ferrite sait déjà traduire la
  seconde : le refus n'était donc pas une limite de tantivy, seulement de son
  agrégation homonyme. Sous une agrégation de buckets, elle reste refusée.
- **`lenient` écarte un champ, il n'avale pas les erreurs.** Une barre de
  recherche pose la même chaîne sur des champs de types différents : `lenient`
  dit « le champ qui ne sait pas lire cette valeur sort de la clause ». La
  tentation est d'attraper *toute* erreur du champ courant — ce serait retourner
  la règle du projet, puisqu'un `slop` non supporté deviendrait un silence dès
  qu'un client passe `lenient: true`. Seule la famille « la valeur n'a pas le
  type du champ » est marquée à la source (`EsError::valeur_illisible`), et
  c'est exactement celle qu'ES avale — mesuré, y compris sur la phrase à
  préfixe posée sur un `keyword`.
- **Trois paramètres de mapping, et aucun n'était une demande vide.**
  `search_analyzer`, `copy_to` et `store` sont ce qui restait entre Wagtail et
  ferrite. Ils ne se ressemblent pas, et chacun a une raison de ne **pas**
  pouvoir être accepté en silence : `search_analyzer` fait chercher le mot
  entier là où on a indexé des grammes (sans lui, `elan` rend tout ce qui
  commence par `e` — chez ES aussi, mesuré), `copy_to` recopie la valeur brute
  dans une cible que la recherche interroge, et `store` conserve la valeur que
  `stored_fields` relit. Ce qui n'était pas devinable : la copie ne se **chaîne
  pas**, sa cible se crée dynamiquement **au type de la valeur copiée**, `fields`
  rend les valeurs copiées bien qu'elles ne soient nulle part dans le `_source`,
  `store: false` n'est pas conservé dans le mapping (comme `index: true`), et un
  champ stocké **sous un `nested`** ne rend rien du tout.
- **`default` n'est pas un analyzer, c'est le nom de celui de l'index.** ES le
  rend tel quel dans `_mapping` dès qu'un champ déclare un `search_analyzer`
  sans analyzer d'indexation. ferrite le relit comme « aucun analyzer déclaré » —
  sans quoi un redémarrage transformerait le `default` qu'ES écrit en
  `standard`, que personne n'a demandé. Le mapping est persisté par son propre
  rendu : tout ce qui n'y fait pas un aller-retour dérive au redémarrage.
- **`missing` sur un `terms` est posé au type du champ.** tantivy sait le faire,
  et c'est une agrégation déléguée de plus : ses bords ne sont pas ceux de son
  homonyme, et les écarts sont **silencieux**. `missing: "2020-01-01"` sur une
  date range les documents sous **1970-01-01** ; `missing: 0` sur un `keyword`
  rend la clé `0` là où ES rend `"0"` ; sur un booléen il ne sait pas la poser.
  ferrite convertit donc la valeur au type du champ avant de la passer, et
  refuse explicitement les deux types que tantivy ne sait pas servir.
- **`minimum_should_match` se calcule, il ne s'approxime pas**
  ([`src/msm.rs`](src/msm.rs)). Ses quatre notations (entier, pourcentage, les
  deux en négatif, et les conditions `3<90%`) tiennent en une trentaine de
  lignes, mais aucun de leurs bords n'était devinable : l'arrondi est une
  **troncature vers zéro** (donc `-33%` de 3 clauses les exige toutes les
  trois, là où un plancher en exigerait 2), un minimum supérieur au nombre de
  clauses n'est **pas** plafonné (`150%` ne rend rien), le séparateur des
  conditions est l'espace, et le `%` doit être le dernier caractère. Tout vient
  de `tests/compat/sonde_msm.py`, qui pose les mêmes questions aux deux
  serveurs. C'est le paramètre que le README du projet cite comme exemple du
  pire échec possible : l'ignorer rendrait **plus** de documents que demandé,
  en silence.

## Les pièges rencontrés, pour ne pas les repayer

- **`search(doc_type=…)` d'un client 7.x écrit dans l'index.** L'URL
  `/{index}/_doc/_search` n'est plus une recherche en 8.x : c'est l'indexation
  d'un document dont l'`_id` est `_search`. Vérifié sur un vrai ES 8.15 **et**
  sur ferrite. À grepper avant toute migration.
- **Un instrument étalonné l'est pour la source contre laquelle on l'a réglé.**
  Le runner de conformance passait 992/1173 contre un vrai ES : branché sur la
  suite d'OpenSearch, il est retombé à 973/978, et les cinq échecs étaient lui.
  Le plus coûteux ne se devinait pas : **OpenSearch a renuméroté à 1.0.0 en
  repartant d'ES 7.10**, et son propre comparateur range les versions *legacy*
  6.x et 7.x **en dessous** de toutes les siennes. Lues comme des nombres, ses
  bornes `skip: {version: " - 7.9.99"}` faisaient sauter 223 cas qu'il joue, et
  `"7.2.0 -"` en faisait jouer deux qu'il saute — lesquels échouaient, ce qui est
  la seule raison pour laquelle on l'a vu. Deux fuites d'état de plus sont
  sorties de la campagne de référence contre un ES **8** : `cat.indices` sans
  `expand_wildcards: all` ne voit pas les index cachés, et `DELETE
  /_component_template/*` échoue **en bloc** dès qu'un seul élément est protégé,
  donc ne supprime rien. Le repli énumère maintenant, et ne touche qu'à ce qui
  n'était pas là au démarrage : un runner défait ce que les cas ont posé, il ne
  démonte pas le serveur qu'on lui prête.
- **Un pré-filtre doit être un sur-ensemble.** Le `nested` cassait sur les
  `must_not` : une négation évaluée à plat écarte un document dont une *autre*
  ligne satisfait la clause.
- **Un `must_not` ne rend pas un `should` facultatif.** Le défaut de
  `minimum_should_match` sous un `nested` valait 0 dès qu'il y avait un
  `must_not` — le `should` était alors purement et simplement jeté. Un document
  dont *une* ligne satisfait le `should` (mais tombe sous le `must_not`) et une
  *autre* ne satisfait rien remontait quand même, là où ES ne le rend pas. Seule
  une clause **obligatoire** (`must`, `filter`) rend le `should` facultatif,
  parce que c'est Lucene qui l'exige : un booléen sans clause requise a besoin
  d'au moins un `should`. Personne ne l'avait signalé — c'est d'avoir mesuré le
  **voisinage** du `minimum_should_match` demandé, jusqu'à sa valeur par défaut,
  qui l'a sorti.

  Et le même piège est revenu un cran plus loin, parce que la correction ne
  portait que sur la valeur **par défaut** : un minimum **explicite** qui
  retombe à zéro — `"50%"` d'une seule clause, la troncature vers zéro d'ES le
  rend nul — ne rend pas non plus le `should` facultatif. ferrite le jetait
  alors entier, et rendait le même document qu'ES ne rend pas. Corriger la
  valeur par défaut d'un paramètre ne corrige pas le paramètre : la règle
  (« au moins une clause positive quand rien n'est obligatoire ») doit être
  appliquée **après** la résolution, pas à sa place. Trouvé par une plage de
  contrôle du fuzzer (graine 4242047), pas par le raisonnement qui avait écrit
  la première correction.
- **Écarter un index n'est pas neutre.** Sur un mapping hétérogène, la première
  version écartait l'index qui ignorait un champ de la requête. Vrai sur un
  `term` seul, faux dans un `bool` : `should: [term sur champ absent, match]`
  rendait 1 document là où ES en rend 163, en silence. La tolérance doit être
  posée **sur la clause**, pas sur l'index. Trouvé par `diff_multi_index.py`,
  pas par le raisonnement qui avait produit le code.
- **Le même piège un cran plus bas : écarter un champ n'est pas neutre.** La
  tolérance au champ non mappé était posée sur la **clause** — juste pour un
  `term`, faux pour un `multi_match` : un seul champ non mappé dans `fields`
  vidait la clause entière, donc **0 document en silence** là où ES ignore ce
  champ-là et cherche dans les autres. C'est le cas d'une barre de recherche qui
  balaie un champ qu'aucun document n'a encore rempli. La tolérance se pose
  toujours au plus près : sur l'élément que le moteur écarte, pas sur ce qui le
  contient. Personne ne l'avait signalé — c'est d'avoir mesuré le **voisinage**
  des deux manques signalés (`lenient`, `type: phrase`) qui l'a sorti.
- **Un `[{…}]` accepté en silence.** `infer` rend `None` sur un objet comme sur
  un tableau d'objets ; seul le premier cas était testé, donc le second entrait
  dans `_source` sans entrer dans le mapping — invisible à la recherche.
- **Un conteneur ES qui vient de démarrer ment.** Un `diff_relevance` à 81/138
  s'est révélé être un ES 8.15 encore en train de se stabiliser. Re-mesurer
  avant de diagnostiquer.
- **La documentation d'un moteur décrit rarement sa version.** La syntaxe
  `regexp` a été écrite d'après ce que Lucene faisait — et `\d` y valait la
  lettre `d`. C'est vrai jusqu'à Lucene 8 ; Lucene 9 (donc ES 8.x) en a fait
  « un chiffre ». Une sonde de vingt lignes contre le conteneur de référence a
  retourné la règle en une minute, là où la lecture donnait une réponse fausse
  avec assurance. Même histoire pour `case_insensitive`, qui ne replie **pas**
  les plages (`[d-e]` ne matche pas `D`), ce qu'aucune documentation ne dit.
- **Une divergence assumée n'est valable que jusqu'au premier vrai client.**
  « Un champ inconnu est une faute de frappe » a tenu tant que personne n'avait
  branché de vraie application : un filtre posé sur *chaque* recherche, sur un
  champ que rien n'a encore mappé, n'est pas une faute — et le 400 rendait
  l'application inutilisable. Quand un écart avec ES est un choix, il faut aussi
  se demander **ce qui arrive à celui qui ne l'a pas fait**.
- **Une fonctionnalité manquante peut en cacher une fausse.** La demande disait
  « `now` échoue en 400 ». La mesure a montré qu'à côté, `lte: "2026-03-15"`
  rendait **moins de documents qu'ES** sans rien dire : ferrite lisait la date
  comme minuit là où ES couvre la journée. Le 400 se voit, l'autre non. Quand un
  client signale un trou, mesurer **tout le voisinage** de ce trou.
- **Un moteur qui rend « la valeur » d'un champ multivalué en choisit une, et
  personne ne dit laquelle.** ferrite triait sur la **première**, ES trie sur le
  minimum en croissant et sur le maximum en décroissant. Même famille : ES ne
  rend pas `null` pour une valeur de tri absente, il rend une **sentinelle**
  (`9223372036854775807`, `"Infinity"`) — et cette sentinelle est une vraie
  valeur, donc un document qui porte `i64::MAX` est ex æquo avec un document
  vide. Aucune de ces règles n'est écrite nulle part ; toutes viennent d'une
  mesure.
- **Une agrégation déléguée n'a pas les mêmes bords que son homonyme.** Celle de
  tantivy **comble les trous** entre deux intervalles d'un `range`, compte les
  **valeurs** là où ES compte les **documents** sur un champ multivalué, nomme
  ses buckets `keyed` autrement, et **fabrique** les buckets vides d'un
  `histogram` sans exécuter ce qu'il y a dessous (une sous-agrégation `range` y
  rendait `buckets: []` là où ES rend ses intervalles à zéro). Quatre résultats
  faux rendus 200. Déléguer une fonctionnalité ne dispense pas de mesurer ses
  bords : c'est même là qu'ils diffèrent.
- **La forme « zéro document » se mesure, elle ne s'écrit pas.** Corriger le
  bucket vide demandait de savoir ce que rend chaque agrégation sur zéro
  document — et l'écrire à la main aurait remis dans le code l'idée qu'on s'en
  fait, une par type d'agrégation, avec ses `extended_bounds` et ses `keyed`. La
  réponse était déjà là : ferrite est **déjà mesuré identique à ES** sur une
  recherche qui ne ramène rien. Les sous-agrégations d'un `histogram` sont donc
  rejouées sur une requête vide, et un bucket à `doc_count: 0` prend cette
  réponse-là — puisque c'est exactement ce qu'il contient.
- **Ce qui lit un champ compte autant que ce qui le filtre.** Un sous-champ de
  `nested` interrogé depuis la racine était refusé dans une **clause** depuis
  longtemps ; la même règle manquait sur l'**agrégation** et sur le **tri**, qui
  lisent les mêmes valeurs à plat. `docs/compat.md` déclarait pourtant les trois
  refusés depuis des mois : une capacité déclarée n'est vraie que si quelque
  chose l'exerce. Le fuzzer pose maintenant les deux formes exprès.
- **Un schéma vide n'échoue pas que sur la forme.** `_validate/query` doit
  séparer « la requête est mal formée » (ES rend `valid: false` sans `_shards`)
  de « elle est impossible sur ce mapping » (ES rend `_shards` et une
  explication par index). Le premier se trouve en construisant la requête contre
  le schéma vide d'`engine::sans_index`, où aucune erreur ne peut venir d'un
  mapping — sauf qu'un `nested` sur un chemin absent y échoue aussi, et sortait
  `valid: false` là où ES dit `true`. Seules les erreurs de **forme**
  (`parsing_exception`, refus explicite) valent verdict de coordinateur. Trouvé
  par le fuzzer le jour où on lui a donné une brique pour cette route — pas par
  le raisonnement qui avait écrit le code.
- **Rendre un compteur à zéro n'est pas neutre.** `_stats` d'ES a une vingtaine
  de groupes ; ferrite en mesure quatre. Les autres à zéro auraient donné un
  `indexing.index_total: 0` sur un index qu'on vient de remplir — « non mesuré »
  rendu comme « aucune activité », et un tableau de bord qui affiche
  tranquillement une courbe plate. Ils sont donc **refusés**, pas rendus. Une
  valeur par défaut plausible est le déguisement le plus efficace d'un échec
  silencieux.
- **Deux outils qui visent le même port se marchent dessus, en silence — et le
  second peut être le binaire qu'on vient de compiler.** Le piège a une seconde
  forme, payée une carte plus tard : un `ferrite` lancé à la main tenait
  toujours le port pendant qu'on en démarrait un neuf ; le neuf a échoué à se
  lier, sans un mot, et **trois mesures d'affilée ont porté sur le binaire
  d'avant** — dont une correction qu'on croyait inopérante et qu'on a cherchée
  ailleurs pendant une demi-heure. Un serveur qui ne répond pas se voit ; un
  serveur qui répond *l'ancienne* réponse, non. La règle : après un
  `cargo build`, vérifier que le processus qu'on interroge est bien celui qu'on
  vient d'écrire (`ferrite.log` porte l'erreur `AddrInUse`, encore faut-il le
  lire).
- **Deux outils qui visent le même port se marchent dessus, en silence.**
  `run.sh` écoute par défaut sur 9200 ; lancé pendant qu'un ferrite y tournait
  déjà, son `bind` a échoué sans bruit et il a exercé **ce** serveur-là — index,
  templates et réglages de cluster compris. La campagne de fuzzing qui tournait
  en parallèle est partie en 400 cas de divergences de mapping qui n'existaient
  pas. Le réflexe de la section 2 s'applique aux outils entre eux : un résultat
  massivement rouge est presque toujours un défaut d'outillage. `run.sh` refuse
  maintenant un port occupé, et rien d'autre ne doit toucher les serveurs
  pendant une campagne.
- **Lire le `_source` n'est pas lire ce qui a été indexé.** `fields` se sert
  dans le `_source` — c'est ce qu'ES fait, et c'est ce qui lui donne l'ordre du
  document et ses doublons. Mais une valeur écartée par `ignore_above` est
  encore dans le `_source` alors qu'elle n'a **pas** été indexée : ES ne la rend
  donc pas dans `fields`, il la rend à part dans `ignored_field_values`. ferrite
  la rendait, en 200, comme si elle était cherchable. Quand on lit une source de
  vérité, se demander laquelle des deux questions on pose.
- **Une sonde différentielle qui ne trouve qu'un serveur annonce « tout
  identique ».** `sonde_fields.py` a rendu « 90/90 identiques » alors que
  ferrite n'écoutait pas : elle ne comparait rien du tout. C'est le même défaut
  d'outillage que la section 2, dans sa forme la plus sournoise — un résultat
  massivement **vert** est aussi suspect qu'un massivement rouge. Toute sonde
  s'arrête maintenant si elle n'a pas ses deux cibles.
- **Un prédicat qui devient plus large en devenant plus lisible.** Ajouter le
  message d'erreur des **deux** serveurs au texte d'un écart de statut — un
  progrès, « statuts 400 / 500 » ne se diagnostique pas — a fait matcher le
  prédicat « refus déclaré » sur un cas où ES échouait **aussi**. Il reconnaît
  la phrase de ferrite ; sa raison d'être est « ferrite refuse là où ES sait
  répondre ». Il lui manquait la seconde moitié. Un prédicat se relit quand ce
  qu'il lit change.
- **Un `curl` de vérification qui n'utilise pas le même texte que le test ne
  vérifie rien.** Une chasse au bug d'analyzer s'est terminée sur un faux
  positif : `match edition` ne trouvait pas `l'édition` — ce que fait aussi ES,
  puisque `standard` garde l'élision. Comparer aux **deux** serveurs avant de
  conclure, y compris quand on croit tenir le coupable.
- **L'ordre des documents de tantivy n'est pas l'ordre d'écriture.** Chez
  Lucene, le numéro interne d'un document (le `_doc` sur lequel un `scroll`
  sans tri ordonne, donc celui qui décide **quels** documents un `max_docs`
  retient) vaut l'ordre d'indexation. Chez tantivy, non : un `_bulk` de 25
  documents ressort en `d002, d000, d003, d001, …`. `_delete_by_query
  ?max_docs=1` supprimait donc *un autre document* qu'Elasticsearch, en 200 et
  sans un mot. La bonne clé était sous la main : le `_seq_no`, attribué sous le
  verrou d'écriture — il **est** l'ordre d'écriture, et il sert déjà de
  condition de concurrence. Trouvé par une plage de graines jamais utilisée du
  fuzzer (2727085), pas par le raisonnement qui avait écrit la première
  version — lequel avait pris pour une garantie ce qui n'était qu'une
  ressemblance entre deux moteurs.
- **Une divergence déclarée a deux faces, et la seconde est l'inverse de la
  première.** `exists` sur un `text` sans terme rend **moins** de documents
  chez ferrite : c'est déclaré, mesuré, et le prédicat du fuzzer l'absorbe
  depuis longtemps. Sous un `must_not`, le même défaut rend **plus** de
  documents — le prédicat, qui ne connaissait qu'un sens, le lisait comme un
  écart réel. Un prédicat écrit sur un signe doit se demander ce qu'une
  négation en fait.
- **Une brique nouvelle réveille du code qui n'avait jamais été poussé.** Les
  n-grammes n'ont rien cassé chez eux ; ils ont sorti trois défauts **ailleurs**,
  tous antérieurs et tous invisibles jusque-là. Un `settings.analysis` posé dans
  un **template** était mis en chaîne par la normalisation des réglages, donc
  illisible au parseur — aucun template ne déclarait d'analyzer avant.
  `PUT /{index}/_mapping` lisait son corps avec une section `analysis` **vide**,
  donc un champ ajouté après coup ne pouvait pas citer un analyzer de son propre
  index. Et `match_phrase` enchaînait les termes d'une même position. Le point
  commun : chacun demandait qu'un analyzer déclaré **et** une seconde
  fonctionnalité se rencontrent, ce qui n'arrivait dans aucun test. Quand on
  livre une brique, il faut chercher ce qui la **traverse**, pas seulement ce
  qui l'appelle.
- **Un refus de trop en cache un autre — trois fois de suite.** Wagtail butait
  sur `analysis.tokenizer` ; une fois celui-là levé, deux refus sont apparus
  qu'aucune des mesures précédentes n'avait pu voir — le préfixe `_` interdit sur
  tout nom de champ, et le `PUT /_mapping` ci-dessus. Une fois les trois
  paramètres de mapping livrés, un quatrième est sorti : `{"bool": {"mustNot":
  …}}`, l'écriture camelCase qu'ES 8.15 **sert encore** (et la seule du DSL :
  `minimumShouldMatch`, `adjustPureNegative`, `maxExpansions`,
  `caseInsensitive`, `tieBreaker`, `scoreMode` sont tous refusés chez lui,
  mesure à l'appui). Une liste de manques établie derrière un mur n'est complète
  que jusqu'au mur. C'est la raison pour laquelle `docs/application.md` publie
  **où l'application s'arrête** et pas seulement combien de ses tests passent.
- **Le préfixe `_` avait été dé-réservé au mapping, pas à la lecture.** Un champ
  utilisateur nommé `_all_text` était accepté à la déclaration, indexé,
  interrogeable — et **invisible** à `fields`, à `docvalue_fields` et au motif
  `*`, qui s'arrêtaient tous au préfixe. En 200, sans un mot, sur les noms
  exacts qu'emploie Wagtail. Lever une règle trop large ne suffit pas : il faut
  chercher tous les endroits qui la répétaient.
- **Un résultat massivement vert n'est pas plus fiable qu'un rouge, et il
  n'alarme personne.** Le premier rapport « Wagtail passe » comptait quatre cas
  de moins du côté d'ES : ils y sortaient `ABSENT` là où ferrite passait, ce qui
  se lit « ferrite fait mieux qu'un vrai Elasticsearch ». C'était le parseur —
  ES rend un en-tête `Warning` de dépréciation sur `mustNot`, `elasticsearch-py`
  l'imprime **au milieu** de la ligne `… ok`, et le verdict tombait à la ligne
  suivante. Le défaut d'outillage ne se manifeste pas toujours par du rouge :
  ici il flattait, et du seul côté qui prévient.
- **`fields` l'emporte sur `stored_fields`, comme sur `docvalue_fields`.** Le
  même champ demandé des deux côtés est rendu par `fields` — donc au `format`
  qu'il demande. ferrite laissait la valeur stockée écraser la valeur formatée,
  en 200. La règle existait déjà pour les colonnes ; livrer une troisième source
  de valeurs, c'est devoir la reposer.
- **`maxTokenLength` ne jette pas, il coupe.** Les tokenizers de Lucene
  découpent un mot de plus de 255 caractères en morceaux de 255, chacun à la
  position suivante — donc tout ce qui suit se décale aussi. ferrite jetait le
  mot entier, et à 255 pile il jetait un mot que Lucene garde. Un texte de plus
  de 255 caractères d'un seul tenant disparaissait donc de l'index, en 200. Ni
  le corpus de `diff_analyzers.py` ni aucun test écrit n'avait un
  mot si long ; il a fallu qu'un `copy_to` fasse entrer un `keyword` de 300
  caractères dans un champ `text` pour qu'un tokenizer le voie. **Une brique
  nouvelle ne mesure pas qu'elle-même** — deuxième fois de suite.
- **`max_expansions` est un budget par position, pas par terme.**
  `MultiPhrasePrefixQuery` remplit **un seul** ensemble en parcourant les termes
  de la position, et s'arrête dès qu'il est plein. Tant qu'un analyzer posait un
  terme par position, les deux lectures se confondaient ; un filtre à n-grammes
  en pose vingt, et un budget par terme développe vingt fois plus de préfixes —
  donc rend plus de documents qu'ES, en 200.
- **Un identifiant numérique n'est pas un identifiant absent.** `_bulk` ne
  lisait `_id` que si c'était une chaîne : `{"_id": 42}` — ce que produit
  `helpers.bulk` du client officiel dès que la clé primaire de l'appelant est un
  entier — tombait donc dans le cas « pas d'identifiant », et le document
  partait sous un identifiant **tiré au sort**, en 201, sans un mot. ES lit
  toute **valeur simple** et la rend en texte (`42` → `"42"`, `true` →
  `"true"`), et refuse un objet ou un tableau en le nommant. Deux choses à
  retenir : le même piège était **déjà corrigé une fonction plus loin**, sur
  `_mget`, commentaire d'explication compris — corriger un lecteur ne corrige
  pas ses voisins ; et c'est la suite du client Python qui l'a trouvé
  (`test_bulk_all_documents_get_inserted`, qui indexe des `_id` de 0 à 99), pas
  un cas écrit ici, parce qu'on écrit ses identifiants entre guillemets sans y
  penser.
- **Lister n'est pas chercher, et un corps juste sous un statut faux ne se voit
  pas.** `GET /_index_template` et `GET /_template` **sans nom** rendaient 404
  sur un serveur neuf, avec le bon corps. ES sépare les deux : sans nom → 200
  même vide, avec un nom ou un motif sans correspondance → 404. Un `curl` ne
  montre rien (le corps est le bon) ; un client qui lève sur 404, si. La mesure
  a demandé un ES démarré **sans ses propres templates** — et elle n'est
  possible que sur la famille `_template`, parce qu'un ES 8.15 réinstalle ses
  templates APM quoi qu'on fasse. Ce qui ne se mesure pas se dit.
- **Un chiffre annoncé que rien n'exerce n'est qu'une phrase.** `GET /_nodes`
  publiait `http.max_content_length_in_bytes: 104857600` — la valeur que les
  clients officiels lisent pour dimensionner leurs lots — pendant que la couche
  HTTP gardait le défaut d'axum, 2 Mo. ferrite annonçait donc cinquante fois ce
  qu'il acceptait, et refusait en `413 text/plain` (hors format d'erreur d'ES)
  un `_bulk` de 5 000 documents : la taille de lot par défaut des tracks Rally,
  et l'ordre de grandeur de `helpers.bulk`. Ni Gitea ni Wagtail ne l'avaient vu,
  parce qu'ils écrivent par petits lots. La constante est maintenant posée à un
  seul endroit et lue par les deux moitiés — et un scénario du harnais envoie
  6 Mo en un appel, parce qu'une constante partagée ne prouve toujours rien
  toute seule.
- **Une agrégation déléguée perd des documents, et seulement dans les buckets
  rares.** Sous un `terms` ou un `range` de premier niveau, la
  **sous-agrégation** de tantivy 0.26.1 ne voyait pas tous les documents de son
  bucket : au-delà de 2 048 documents en cache, `LowCardSubAggCache::flush_local`
  ne recopiait que les buckets au-dessus d'un seuil puis effaçait le cache
  entier. Les `doc_count` restaient justes — donc la réponse avait l'air bonne,
  en 200. Deux raisons pour lesquelles c'est resté invisible : il faut plus de
  2 048 documents **par segment** (les 600 de `diff_aggs.py` n'y arrivent pas,
  ni les 25 du fuzzer), et il faut un bucket **rare** (un corpus régulier donne
  à chaque bucket sa part de chaque tranche, donc au-dessus du seuil). C'est la
  pire forme du défaut : ce qui disparaît est la minorité. Corrigé en épinglant
  le correctif d'amont — et trois choses de cette correction méritent d'être
  retenues, parce qu'aucune ne se lisait dans le code de la dépendance :
  - **la borne d'un défaut se reproduit, elle ne s'estime pas.** « ~2 048 » est
    devenu « 2 047 juste, 2 048 faux », et « les buckets rares » est devenu
    « au plus `2048 / (2 × nombre de buckets)` documents dans la fenêtre » —
    204 perdus, 205 gardés. La formule, pas un ordre de grandeur ;
  - **une liste de symptômes établie sur un symptôme est incomplète.** Ce qui
    était publié disait « un `value_count` de 1 692 ». La matrice complète
    (5 parents × 9 sous-agrégations) a montré que **toutes** les métriques
    étaient touchées et les sous-agrégations de buckets aussi : 14 formes
    fausses sur 46. La pire n'était pas la plus visible — `avg` rendait 21,5 au
    lieu de 21,428…, un nombre faux *plausible* ;
  - **une épingle sur un fork est une dette qu'il faut rendre mesurable.**
    `[patch.crates-io]` remplace **neuf** paquets d'un coup. D'où
    `verifie_tantivy.py`, qui télécharge les crates publiées et prouve que le
    fork en est l'égal à un fichier près, et `tests/spike_sous_aggs.rs`, qui
    casse dans `cargo test` si l'épingle saute. Le chemin de sortie est écrit
    d'avance : [`docs/tantivy-patch.md`](docs/tantivy-patch.md).
- **Un cliquet qui bat, et une hypothèse qui coûtait la mesure.** La CI d'une PR
  est passée rouge puis verte sans qu'une ligne ne bouge : un cas de
  `indices.stats` tombait de `refus` à `echec` sur `[index] 404 : no such index
  [test1]`, une fois sur dix-sept campagnes. Trois leçons, dans l'ordre où elles
  se sont payées. **(1)** L'hypothèse écrite sur la carte — « un alias survit à
  `nettoie()` » — était fausse, et c'est un mode qui *vérifie* l'état entre deux
  cas (aucun index, alias, template, réglage de cluster) qui l'a éliminée : il
  est resté vert pendant une campagne qui a basculé. **(2)** Le 404 était un
  **masque** : `get_or_create` répondait « no such index » dès que la création
  échouait, quelle qu'en soit la raison — un `Err(_) => self.get(name)` écrit
  pour le seul cas « un autre appel a gagné la course ». Rendre l'erreur réelle
  a nommé la cause en deux campagnes. **(3)** La cause n'était pas dans le
  runner mais dans ferrite : `refresh_dirty` travaille sur un **instantané** du
  catalogue, donc elle tient l'`Arc` d'un index que `DELETE` vient de retirer —
  et ses répertoires (`{index}/index-0`) sont exactement ceux que l'index
  homonyme recréé juste après s'attribue. Le vieux balayage efface la génération
  vivante du neuf. Un index supprimé est donc **marqué**, et la suppression
  libère le nom par un renommage atomique sous `.corbeille/` : plus aucun chemin
  n'est partagé entre un index et son successeur. Retirer d'une table n'est pas
  tuer — tant qu'un `Arc` vit, il faut lui retirer le droit d'écrire.
- **« Les termes de la requête » n'est pas « ce qui a fait correspondre ce
  document ».** Le surlignage d'ES ne marque que ce qui a vraiment contribué :
  un `should` placé dans un `bool` dont le `filter` échoue ne marque rien, et un
  `bool` porteur d'un `must_not: {match_all}` ne marque jamais rien — Lucene le
  réécrit en `MatchNoDocsQuery`. Une extraction à plat des termes de la requête
  marquait les deux, en 200. Il a donc fallu garder la **forme booléenne** de la
  requête et l'évaluer document par document. Et la règle a une seconde moitié
  qui la retourne : sous `require_field_match: false`, ES abandonne les
  `Matches` du champ et repart de l'extraction statique — donc le tri par
  document **disparaît**, et une phrase y est marquée terme par terme au lieu
  d'une seule fois. Trouvé par le fuzzer (graines 6, 106, puis 900186 sur une
  plage de contrôle), jamais par les 192 questions écrites à la main.
- **Un `BreakIterator` de Java n'est pas UAX#29.** Le découpage des fragments
  s'appuie sur les frontières de mot du JDK, et elles divergent de la norme sur
  des caractères courants : `abcde-fghij` et `abcde"fghij` sont **un** mot,
  `abcde:fghij` et `abcde’fghij` en font deux — l'inverse de ce que dit UAX#29
  pour les deux premiers. Implémenter la norme donnait « tiret- » là où ES rend
  « tiret-bas ». La seule façon de le savoir a été de poser `no_match_size: 1`
  sur seize mots construits exprès et de lire où tombait la coupure.
- **Une clé de bucket entière sur un champ flottant.** `terms` sur un `double`
  rendait la clé `2` là où ES rend `2.0` — un client qui type strictement son
  JSON y lit un entier. Défaut antérieur, invisible parce qu'aucun corpus écrit
  à la main ne met une valeur entière dans un champ flottant ; le fuzzer, lui,
  tire `0.0` et `1024.0` exprès.
- **Le chiffre le plus facile à vérifier est celui qui dérive le plus vite.**
  La taille d'image est le premier argument du projet, et la seule mesure qui ne
  passait par aucun des neuf gestes : elle lisait un champ d'un outil
  (`docker image inspect --format '{{.Size}}'`) au lieu de compter des octets.
  Ce champ a **changé de définition** avec le magasin d'images — somme des
  couches décompressées avec le magasin classique, somme des blobs compressés
  avec celui de containerd, le défaut depuis Docker 29 : le même script est
  passé de 8,2 à 3,8 sans qu'une ligne bouge, pendant que `docker images`
  affichait 13,5 pour la même image — sa colonne `DISK USAGE` additionnant
  encore autre chose. Et la ligne publiée comparait 638 Mo à 8,2 Mo, c'est-à-dire
  la taille **compressée** d'Elasticsearch à la taille **décompressée** de
  ferrite, toutes deux en Mio sous le nom de Mo. Trois leçons, et aucune ne
  porte sur Docker : un chiffre publié doit dire **de quoi il est la mesure** ;
  une comparaison n'a de sens que si les deux côtés portent la même définition
  (et sont mesurés par le **même outil**, ce qui est la seule façon de s'en
  assurer) ; et un outil de mesure ne doit pas lire un champ dont il ne contrôle
  pas le sens — `measure_container.sh` demande maintenant l'image (`docker save`,
  format OCI) et compte les octets, en imprimant quand même la version de Docker
  puisqu'elle change ce que les *autres* outils répondent.

  Et la correction a repayé le défaut qu'elle corrigeait, au premier passage en
  CI. `docker save` ne rend les blobs **compressés** que depuis le magasin de
  containerd ; les runners sont en Docker 28, où il écrit bien un layout OCI mais
  avec des **couches nues** et un manifeste qui déclare leur taille décompressée.
  Le script a donc publié 9 520 806 octets en « compressée (registre) » — la
  taille décompressée sous le nom de l'autre, dans un job **vert**. Un format
  reconnu n'est pas une garantie sur ce qu'il contient : la question se pose aux
  octets de chaque couche, et une couche nue rend la taille du registre **non
  déductible**, donc refusée (code de retour non nul) plutôt que remplacée par un
  nombre plausible. Le chiffre publié se mesure maintenant sur l'artefact OCI de
  buildx — ce qu'un `docker push` enverrait — étalonné contre le premier chemin :
  le **blob de la couche est identique à l'octet** (4 005 821), et les totaux ne
  diffèrent que de 6 octets, tous dans le JSON de configuration, parce que deux
  builds distincts n'y écrivent pas un horodatage de la même longueur. C'est
  cette mesure-là, et pas la ressemblance des deux commandes, qui autorise à
  croire le second.

  **Et une mesure juste qui n'existe que dans un terminal ne protège de rien.**
  Une fois le protocole réparé, le chiffre restait recopié à la main : la page
  produit annonçait encore 2,4 Mo (22 occurrences) pendant que la mesure disait
  4,0, et le README portait des nombres qu'aucun outil ne relisait. C'est le
  même défaut que `docs/compat.md` tenu à la main — la source est maintenant
  [`docs/container.json`](docs/container.json), écrit par la campagne, où
  **chaque valeur porte sa définition** ; le README et
  [`docs/bench.md`](docs/bench.md) en sont générés, et la CI échoue s'ils
  divergent. Le cliquet a une seconde moitié qui compte autant : un marqueur ou
  un motif introuvable est une **erreur**, sinon supprimer une ligne du README
  ferait passer le contrôle au vert. Et la version de `Cargo.toml` est comparée
  à celle du rapport — sans quoi un binaire qui grossit republierait la taille
  de l'ancien sous son nouveau numéro.

## Où va le projet

**Deux vraies applications tournent dessus sans être modifiées** : Gitea
v1.27.2 y indexe et cherche ses issues (34 cas, les mêmes que contre un vrai
ES 8.15), et **Wagtail v7.1 passe les 83 tests de sa suite de backend
Elasticsearch** — il en passait zéro il y a trois cartes. Le mouchard ne relève
plus un seul refus que ferrite prononce là où ES sait répondre. Le chemin est
dans [`docs/application.md`](docs/application.md), et il vaut plus que le
chiffre : le blocage est tombé d'un cran à chaque carte, et **à chaque fois le
suivant était un refus de trop** plutôt qu'un manque.

**L'autocomplétion « au fil de la frappe »** est complète : `ngram` et
`edge_ngram` — tokenizer et filtre, déclarés dans `settings.analysis` et bornés
par `index.max_ngram_diff` — travaillent à l'**indexation**, et
`search_analyzer` fait chercher le **mot entier** par-dessus. Les deux moitiés
comptent autant : sans la seconde, `elan` rend tout ce qui commence par `e`, et
c'est ce que fait ES aussi tant qu'on ne le lui dit pas.

**Se refaire un `_all`** est possible : `copy_to` recopie la valeur brute d'un
champ dans une ou plusieurs cibles à l'indexation, la cible se crée toute seule
si le mapping ne la déclare pas, et une facette peut ranger sous une clé les
documents qui n'ont pas le champ (`missing` sur un `terms`). Et `store: true`
donne enfin quelque chose à lire à `stored_fields`, qui ne rendait rien — c'est
ainsi qu'une application relit une seule clé sans rapatrier tout le `_source`.

Une migration depuis une instance 7.10.2 se reprend maintenant **entière** sur
l'index d'exemple, et un projet qui découpe ses données en plusieurs index —
catalogues séparés, index quotidiens derrière un alias — se branche sans
changer son code. Les filtres « contient / commence par / finit par » d'un
service (`regexp`, `case_insensitive`), l'autocomplétion (`match_phrase_prefix`)
et le `DELETE /*` d'un script d'init (sous `PUT /_cluster/settings`) passent
aussi tels quels.

L'export d'un index par `helpers.scan` (donc une sauvegarde, donc un
`timemachine export`) passe maintenant, et une application qui compte ses
filtres rapides en agrégations `filter` sur chaque appel aussi.

Un tableau de bord qui filtre sur `now` (« en retard », « livré aujourd'hui »,
« ce mois-ci ») se branche aussi : le date math est résolu côté serveur, et une
borne de date est arrondie du bon côté.

La **recherche libre** d'une application — un `multi_match` qui balaie
identifiant, référence et nom du client d'un coup — passe maintenant telle
quelle : `lenient` écarte les champs dont le type ne sait pas lire ce qu'on
tape, `type: phrase` et `type: phrase_prefix` cherchent l'expression exacte ou
son début, et un champ pas encore mappé n'annule plus la clause. Le
`minimum_should_match` qui l'accompagne — « au moins les trois quarts des
critères », écrit `"75%"` — est calculé comme chez ES, dans ses quatre
notations, sur un `bool` comme sous un `nested`.

Un outil de découverte de champs, un script d'init qui pose un template et un
tableau de bord qui lit `_stats` se branchent aussi : `_field_caps`,
`_validate/query`, `_stats`, `PUT /{index}/_settings` et les templates d'index
(les deux familles, `_index_template` et le `_template` des scripts venus de la
7.x) sont servis. `_stats` ne rend que les quatre groupes que ferrite **mesure**
et refuse les autres : un `index_total: 0` sur un index qu'on vient de remplir
ferait passer « non mesuré » pour « aucune activité ».

Une réponse ne transporte plus forcément tout le `_source` : `fields`,
`docvalue_fields` et `stored_fields` sont servis. Les trois ne lisent pas au
même endroit, et c'est tout le sujet — le `_source` pour `fields` (donc l'ordre
du document et ses doublons), les colonnes pour `docvalue_fields` (donc trié, et
dédoublonné sur un `keyword`), les champs stockés pour `stored_fields` (donc
rien, puisque `store` est refusé au mapping — exactement ce que rend un ES dont
le mapping ne le porte pas). Ce qui compte pour un client, c'est la **forme** :
chaque valeur est un tableau, même mono-valuée, et un champ absent n'a pas de
clé. `script_fields` et `runtime_mappings` restent refusés — leur objet **vide**
est accepté, parce qu'il ne demande rien.

**Modifier ou purger par requête** se fait maintenant côté serveur :
`_delete_by_query` (purger un locataire, retirer un lot par filtre) et
`_update_by_query`, qui **sans script** réindexe depuis le `_source` — le geste
d'après un `PUT /_mapping`. Les compteurs sont ceux d'ES, `version_conflicts`
compris, et ils ne sont pas décoratifs : la commande relève chaque document sur
l'instantané de la recherche puis n'écrit **que s'il n'a pas bougé depuis**.
`_reindex` reste hors périmètre, et c'est le seul des trois : la copie d'un
index vers un autre s'écrit encore côté client avec `scroll` + `_bulk`, ce que
les deux autres ne permettaient pas.

**Une barre de recherche rend enfin ses extraits.** `highlight` est servi, et
ce qui a coûté le travail n'est pas de marquer les termes : c'est de couper les
fragments **là où Lucene les coupe**. Ni « une phrase », ni « `fragment_size`
caractères » — les phrases sont fusionnées vers l'avant tant que la longueur
tient sous la borne, puis re-coupées au mot ; un point suivi d'une minuscule
n'en termine pas une ; le fragment se centre sur le **milieu** de la
correspondance ; et quand il y en a plus que `number_of_fragments`, ce sont les
mieux notés par le `PassageScorer` de Lucene qui restent, remis dans l'ordre du
document. `type`, `highlight_query`, `matched_fields`, `boundary_scanner`,
`encoder`, `fragmenter` et `order: score` sont refusés en les nommant.
C'était le **rang 4 mesuré** du corpus d'usage, et la raison pour laquelle
ReadTheDocs avait été écarté des applications réelles — il ne lui reste que
`inner_hits`.

Ce qui reste, par ordre de gêne pour un projet réel : `rest_total_hits_as_int`,
`_msearch`, `_reindex`, les templates de **composants** (`_component_template`, et le
`composed_of` qui les cite — refusé à la pose plutôt qu'appliqué à moitié),
`inner_hits`, `GET /_cat/aliases` et les colonnes `h` / `s` des `_cat`,
`GET /{index}/_mapping/field/{champs}`, l'agrégation `filters` (la sœur
plurielle de `filter`), `time_zone` sur un `range` (refusé explicitement), les
alias **filtrés** (`filter`, refusé explicitement), `?stored_fields=` sur
`GET /{index}/_doc/{id}` (le geste se fait par `_search`), et les analyzers des
autres langues.

Le seul échec silencieux connu du projet est **corrigé** : une recherche qui ne
visait **aucun index** (cluster vide, ou motif sans correspondance) rendait 200
sans valider son corps, parce que la traduction du Query DSL se fait index par
index. Elle est maintenant exercée contre un **schéma vide**
(`engine::sans_index`) avant qu'on conclue qu'il n'y a rien à chercher — et avec
elle les agrégations et le tri. Ses deux petits frères aussi : `include_defaults`
et `flat_settings` sont refusés au lieu d'être acceptés et ignorés, sur les
réglages d'index comme sur ceux du cluster.

Les trois derniers manques de cette liste à avoir été **mesurés** plutôt que
supposés viennent des 85 domaines de conformance qu'on ne lançait pas :
`_cat/aliases` (10 cas), `_mapping/field` (15 cas), et `remove_index` posé en
même temps qu'un alias du même nom (1 cas). Voir
[`docs/conformance.md`](docs/conformance.md).

**Écrire un alias se fait maintenant par les sept URL d'ES**, pas seulement par
celle qui porte son nom dans le chemin : le nom de l'alias, celui de l'index, ou
les deux, peuvent venir du corps, et le corps **remplace** le chemin. `must_exist`
est lu sur un `remove`, et un `remove` désigne des alias (`logs-*`, `_all`) au
lieu de les nommer. Ces quatre-là ne venaient d'aucune demande : ils viennent
des deux suites que ferrite ne contrôle pas — trois de celle d'OpenSearch, la
quatrième (`?timeout=` sur `_search`, accepté et sans objet comme `preference`)
de la suite d'intégration du client go. Aucune n'était visible à la suite
d'Elastic, figée en 7.10.2. `include_named_queries_score` reste **refusé et
nommé** : il ne change que la forme de `matched_queries`, que ferrite ne rend
pas, et le servir à moitié aurait été pire que ne pas le servir.

## Ton, et forme des livrables

Le dépôt est en français, y compris les messages de commit et la documentation.
Les commentaires de code évitent les accents (le reste non). Un commit explique
**pourquoi**, pas seulement quoi, et cite les mesures constatées plutôt que
« tests OK ». Une PR qui change un comportement met à jour `docs/compat.md` dans
la même PR.
