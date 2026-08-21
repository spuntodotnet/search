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

## La méthode, en sept gestes

Ces sept gestes ont chacun trouvé quelque chose qu'un raisonnement n'aurait pas
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

### 8. Brancher un logiciel que personne ici n'a écrit

Les sept gestes précédents mesurent des **surfaces d'API**, avec trois
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

## Les outils, et ce que chacun répond

Tous depuis la racine. Les diffs exigent Docker (ce sont des outils de
développement, pas de CI).

| Commande | La question à laquelle elle répond |
|---|---|
| `./tests/compat/run.sh` | est-ce que le client officiel 8.x fait tout ce qu'on prétend ? (**107/107**, dont l'export par `helpers.scan`, le date math, la recherche libre, l'expression de noms d'alias, la recherche sans index, `_field_caps`, `_validate/query`, `_stats`, les templates, ce que la réponse transporte — `fields`, `docvalue_fields`, `stored_fields` — la modification par requête, `_delete_by_query` / `_update_by_query`, et les n-grammes de l'autocomplétion) |
| `tests/compat/diff_relevance.py` | **les mêmes documents dans le même ordre** qu'ES ? (212/213, 0 écart réel) |
| `tests/compat/diff_against_es.py` | la même *forme* de réponse ? (45/46 ; le seul écart est `_cluster/health`, toujours vert par choix) |
| `tests/compat/diff_aggs.py` | les mêmes agrégations ? (53/53, `filter` comprise, et ce qu'un bucket **vide** doit porter) |
| `tests/compat/diff_analyzers.py` | les mêmes tokens, **aux mêmes positions et aux mêmes offsets** ? (38 batteries × 210 textes : 7 analyzers intégrés, 21 déclarations de n-grammes, les 5 analyzers de Wagtail, et les 5 classes de `token_chars` demandées caractère par caractère — toutes identiques) |
| `tests/compat/diff_datemath.py` | les mêmes documents sur une **borne de date** — `now`, `now-1d/d`, `2026-03-15\|\|+1M`, et l'arrondi selon le côté de la borne ? (276/276, messages d'erreur compris ; 45/276 avant le chantier) |
| `tests/compat/diff_motifs.py` | les mêmes documents sur un **motif** — `regexp`, `wildcard`, `prefix`, `match_phrase_prefix` ? (101/101) |
| `tests/compat/diff_multi_index.py` | `index=["a","b"]`, `logs-*`, les alias : **les mêmes index visés, fusionnés pareil** ? (87/87, 0 écart, plus aucune divergence assumée ; `--calibrer` : 87/87 contre deux ES) |
| `tests/compat/sonde_msm.py` | les mêmes documents sur un **`minimum_should_match`** — entier, pourcentage, formes négatives, conditions `3<90%`, et sous un `nested` ? (53/53) |
| `tests/compat/releve_mots_vides.py` | quelle est **vraiment** la liste de mots vides d'un analyzer d'ES ? |
| `tests/compat/sonde_fields.py` | **ce que la réponse transporte** — `fields`, `docvalue_fields`, `stored_fields`. Compare le **hit entier** (bloc `fields` clé par clé, présence de `_source`, présence de `_id`) : 94/96 identiques, 2 refus assumés écrits, 0 écart. Refuse de tourner si elle ne trouve pas les deux serveurs |
| `tests/compat/sonde_par_requete.py` | **modifier ou purger par requête** — `_delete_by_query`, `_update_by_query`. Compare les compteurs de la réponse **et l'état laissé derrière** (documents restants, `_version`, `_source`) : 62/74 identiques, 12 refus assumés écrits, 0 écart. Les conflits sont provoqués pour de vrai, par une écriture non rafraîchie. Refuse de tourner sans ses deux cibles |
| `tests/compat/sonde_alias.py` | les mêmes alias sur une **expression de noms** — liste, joker, exclusion, `_all` — et le même 404 ? (21/21, corps et message compris) |
| `tests/compat/sonde_vide.py` | sur un serveur **sans aucun index**, la même chose qu'ES — et rien accepté en silence ? (28/28 identiques, 0 refus muet ; les deux serveurs doivent être vides, c'est l'état mesuré) |
| `tests/compat/fuzz_vs_es.py` | et **en dehors** des combinaisons auxquelles on a pensé ? Mapping, documents et requêtes tirés au sort dans le périmètre déclaré (`compat.yaml` dit ce qui est jouable), posés aux deux serveurs. **3 200 cas, 141 260 requêtes, 1 divergence réelle** (un ordre que BM25 sépare, ouverte et décrite dans [`docs/fuzz.md`](docs/fuzz.md)), sur douze plages de graines dont **six** n'ont jamais servi à corriger — celle sur laquelle on itère ne mesure plus rien. 21 défauts silencieux trouvés au premier passage, 4 de plus depuis, dont un `max_docs` qui ne supprimait pas les mêmes documents qu'ES. S'étalonne contre **deux** Elasticsearch avant de servir : `--calibrer` (60 cas, 2 532 requêtes, 0) |
| `tests/compat/sonde_fuzz.py` | les écarts trouvés par le fuzzing, **figés** hors d'une graine (46/46, plus 12 refus assumés) |
| `tests/compat/appli_reelle.py` | **un logiciel écrit par d'autres démarre-t-il ?** Clone une vraie application à une révision figée, vérifie que rien n'y a bougé, lance sa **propre** suite d'intégration contre un vrai ES puis contre ferrite, et relève tout le trafic HTTP au passage. Gitea v1.27.2 : **34/34 des deux côtés**, arbre intact. Wagtail v7.1 : **0/81**, mais son index se crée maintenant — il reste trois paramètres de mapping. Voir [`docs/application.md`](docs/application.md) |
| `tests/compat/genere_compat.py` | le périmètre déclaré et la doc disent-ils la **même chose** ? [`compat.yaml`](compat.yaml) est la source (une entrée par capacité : état, paramètres, motif du refus, poids d'usage) ; [`docs/compat.md`](docs/compat.md) et [`docs/compat.json`](docs/compat.json) en sont **générés**, et la CI échoue s'ils divergent |
| `tests/compat/perimetre.py` | ce cas qui échoue, il porte sur quoi ? Il rattache un échec de conformance à une capacité déclarée : **régression** si elle est annoncée supportée, **coût de périmètre** si elle est annoncée refusée |
| `tests/compat/recolte_usage.py` | à quoi ressemblent les requêtes que les gens envoient **vraiment** ? Constitue le corpus ([`tests/compat/usage/corpus.jsonl`](tests/compat/usage/corpus.jsonl), 5 311 requêtes) depuis quatre sources citables : doc de référence 8.15, tracks Rally, clients officiels, code open source. Chaque requête porte l'URL d'où elle vient |
| `tests/compat/ponderation.py` | **quelle part de ces requêtes passe entièrement ?** (42,1 % du corpus, mais **93,2 % du code d'application** et 27,2 % des tracks Rally — l'écart *est* le résultat). Écrit les `poids` de `compat.yaml`, publie [`docs/usage.json`](docs/usage.json) et la table « ce qui manque, par fréquence d'usage ». `--rejoue` pose la même requête à ferrite et à un vrai ES 8.15 : les deux mesures s'accordent sur 99,3 % des cas |
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
  transforme « 356 échecs » en « 40 régressions et 316 coûts de périmètre »
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
- **Un refus de trop en cache un autre.** Wagtail butait sur `analysis.tokenizer`
  ; une fois celui-là levé, deux refus sont apparus qu'aucune des mesures
  précédentes n'avait pu voir — le préfixe `_` interdit sur tout nom de champ, et
  le `PUT /_mapping` ci-dessus. Une liste de manques établie derrière un mur
  n'est complète que jusqu'au mur. C'est la raison pour laquelle
  `docs/application.md` publie **où l'application s'arrête** et pas seulement
  combien de ses tests passent.

## Où va le projet

**Une vraie application tourne dessus sans être modifiée** : Gitea v1.27.2 y
indexe et cherche ses issues, et sa suite d'intégration passe ses 34 cas — les
mêmes que contre un vrai ES 8.15. La seconde cible mesurée, Wagtail v7.1, ne
passe toujours **aucun** de ses 81 tests de backend — mais son index **se crée**
maintenant : les n-grammes qui le bloquaient sont livrés, et ce qui reste tient
en trois paramètres de mapping (`search_analyzer`, `copy_to`, `store`), contre
six lignes auparavant. Les deux résultats sont dans
[`docs/application.md`](docs/application.md) — celui qui échoue est une feuille
de route sourcée, pas un aveu.

**L'autocomplétion « au fil de la frappe »** est là : `ngram` et `edge_ngram`,
tokenizer et filtre, déclarés dans `settings.analysis` et bornés par
`index.max_ngram_diff`. Ils travaillent à l'**indexation**, là où
`match_phrase_prefix` travaille à la requête — un CMS qui propose des pages
pendant qu'on tape n'a pas d'autre moyen.

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

Ce qui reste, par ordre de gêne pour un projet réel : `rest_total_hits_as_int`,
`_msearch`, `_reindex`, les templates de **composants** (`_component_template`, et le
`composed_of` qui les cite — refusé à la pose plutôt qu'appliqué à moitié),
`GET /_cat/aliases` et les colonnes `h` / `s` des `_cat`,
`GET /{index}/_mapping/field/{champs}`, l'agrégation `filters` (la sœur
plurielle de `filter`), `time_zone` sur un `range` (refusé explicitement), les
alias **filtrés** (`filter`, refusé explicitement), les trois paramètres de
mapping qui restent à Wagtail (`search_analyzer`, `copy_to`, `store`), et les
analyzers des autres langues.

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
