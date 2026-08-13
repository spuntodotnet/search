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
image de 8,2 Mo. Le produit, c'est **« le code client existant ne change pas »**.
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

## La méthode, en six gestes

Ces six gestes ont chacun trouvé quelque chose qu'un raisonnement n'aurait pas
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

### 4. Prendre les tests des autres — et ne pas choisir lesquels

Le harnais maison teste ce à quoi on a pensé. La suite REST d'Elasticsearch
teste ce à quoi *Elastic* a pensé — et c'est elle qui a trouvé les deux vrais
manques (création d'index à l'écriture, routes sans index) qu'aucun test écrit
ici n'avait vus. Voir [`docs/conformance.md`](docs/conformance.md).

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

### 6. Vérifier une hypothèse sur une dépendance par un spike, pas par une lecture

`tests/spike_nested.rs` mesure deux propriétés de tantivy dont dépend tout le
support de `nested`. Elles ne sont pas documentées comme des garanties : le
spike les verrouille, et cassera bruyamment à la montée de version plutôt que le
jour où on s'appuiera dessus.

## Les outils, et ce que chacun répond

Tous depuis la racine. Les diffs exigent Docker (ce sont des outils de
développement, pas de CI).

| Commande | La question à laquelle elle répond |
|---|---|
| `./tests/compat/run.sh` | est-ce que le client officiel 8.x fait tout ce qu'on prétend ? (**92/92**, dont l'export par `helpers.scan`, le date math, la recherche libre, l'expression de noms d'alias et la recherche sans index) |
| `tests/compat/diff_relevance.py` | **les mêmes documents dans le même ordre** qu'ES ? (212/213, 0 écart réel) |
| `tests/compat/diff_against_es.py` | la même *forme* de réponse ? (45/46 ; le seul écart est `_cluster/health`, toujours vert par choix) |
| `tests/compat/diff_aggs.py` | les mêmes agrégations ? (45/45, `filter` comprise) |
| `tests/compat/diff_analyzers.py` | les mêmes tokens ? (7 analyzers, 210 textes, tous identiques) |
| `tests/compat/diff_datemath.py` | les mêmes documents sur une **borne de date** — `now`, `now-1d/d`, `2026-03-15\|\|+1M`, et l'arrondi selon le côté de la borne ? (276/276, messages d'erreur compris ; 45/276 avant le chantier) |
| `tests/compat/diff_motifs.py` | les mêmes documents sur un **motif** — `regexp`, `wildcard`, `prefix`, `match_phrase_prefix` ? (101/101) |
| `tests/compat/diff_multi_index.py` | `index=["a","b"]`, `logs-*`, les alias : **les mêmes index visés, fusionnés pareil** ? (87/87, 0 écart, plus aucune divergence assumée ; `--calibrer` : 87/87 contre deux ES) |
| `tests/compat/sonde_msm.py` | les mêmes documents sur un **`minimum_should_match`** — entier, pourcentage, formes négatives, conditions `3<90%`, et sous un `nested` ? (47/47) |
| `tests/compat/releve_mots_vides.py` | quelle est **vraiment** la liste de mots vides d'un analyzer d'ES ? |
| `tests/compat/sonde_alias.py` | les mêmes alias sur une **expression de noms** — liste, joker, exclusion, `_all` — et le même 404 ? (21/21, corps et message compris) |
| `tests/compat/sonde_vide.py` | sur un serveur **sans aucun index**, la même chose qu'ES — et rien accepté en silence ? (27/27 identiques, 0 refus muet ; les deux serveurs doivent être vides, c'est l'état mesuré) |
| `tests/compat/genere_compat.py` | le périmètre déclaré et la doc disent-ils la **même chose** ? [`compat.yaml`](compat.yaml) est la source (une entrée par capacité : état, paramètres, motif du refus, poids d'usage) ; [`docs/compat.md`](docs/compat.md) et [`docs/compat.json`](docs/compat.json) en sont **générés**, et la CI échoue s'ils divergent |
| `tests/compat/perimetre.py` | ce cas qui échoue, il porte sur quoi ? Il rattache un échec de conformance à une capacité déclarée : **régression** si elle est annoncée supportée, **coût de périmètre** si elle est annoncée refusée |
| `tests/compat/recolte_usage.py` | à quoi ressemblent les requêtes que les gens envoient **vraiment** ? Constitue le corpus ([`tests/compat/usage/corpus.jsonl`](tests/compat/usage/corpus.jsonl), 5 311 requêtes) depuis quatre sources citables : doc de référence 8.15, tracks Rally, clients officiels, code open source. Chaque requête porte l'URL d'où elle vient |
| `tests/compat/ponderation.py` | **quelle part de ces requêtes passe entièrement ?** (36,3 % du corpus, mais **89,6 % du code d'application** et 16,6 % des tracks Rally — l'écart *est* le résultat). Écrit les `poids` de `compat.yaml`, publie [`docs/usage.json`](docs/usage.json) et la table « ce qui manque, par fréquence d'usage ». `--rejoue` pose la même requête à ferrite et à un vrai ES 8.15 : les deux mesures s'accordent sur 99,3 % des cas |
| `tests/compat/conformance_es.py` | que dit la suite de tests **d'Elastic** ? Ses **107 domaines**, sans liste blanche. Son rapport est un fichier, pas une phrase : [`docs/conformance.json`](docs/conformance.json) (totaux, deux taux, exclusions comptées, détail par cas), régénéré par `--json`, tenu par un cliquet en CI (`--diff`) |
| `tests/compat/bench_vs_es.py` | mêmes résultats, **et à quel prix** ? (×3,6 en latence, ×6 en indexation) |
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
  transforme « 393 échecs » en « 37 régressions et 356 coûts de périmètre »
  (la mesure du jour, dans [`docs/conformance.json`](docs/conformance.json)) :
  la différence entre un chiffre qu'on subit et un chiffre qu'on pilote. Le
  garde-fou est le troisième verdict : un cas qu'aucune capacité ne réclame
  compte **contre** nous, sinon oublier de déclarer une capacité ferait monter
  le taux.
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
  `stop`).
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
- **Un `curl` de vérification qui n'utilise pas le même texte que le test ne
  vérifie rien.** Une chasse au bug d'analyzer s'est terminée sur un faux
  positif : `match edition` ne trouvait pas `l'édition` — ce que fait aussi ES,
  puisque `standard` garde l'élision. Comparer aux **deux** serveurs avant de
  conclure, y compris quand on croit tenir le coupable.

## Où va le projet

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

Ce qui reste, par ordre de gêne pour un projet réel : `rest_total_hits_as_int`,
`_msearch`, `_stats`, les templates, `PUT /{index}/_settings`, `GET /_cat/aliases`
et les colonnes `h` / `s` des `_cat`, `GET /{index}/_mapping/field/{champs}`,
l'agrégation `filters` (la sœur plurielle de `filter`), `time_zone` sur un
`range` (refusé explicitement), les alias **filtrés** (`filter`, refusé
explicitement), et les analyzers des autres langues.

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

## Ton, et forme des livrables

Le dépôt est en français, y compris les messages de commit et la documentation.
Les commentaires de code évitent les accents (le reste non). Un commit explique
**pourquoi**, pas seulement quoi, et cite les mesures constatées plutôt que
« tests OK ». Une PR qui change un comportement met à jour `docs/compat.md` dans
la même PR.
