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
image de 2,4 Mo. Le produit, c'est **« le code client existant ne change pas »**.
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

## La méthode, en cinq gestes

Ces cinq gestes ont chacun trouvé quelque chose qu'un raisonnement n'aurait pas
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
il passe 537/643 contre ES — et ses verdicts sur ferrite veulent enfin dire
quelque chose.

Même piège deux fois : le nettoyage entre deux cas échouait (une fois sur un
index en lecture seule côté ES, une fois sur un joker que ferrite refuse), et
**tout cascadait ensuite en « index already exists »**. Un résultat massivement
rouge est presque toujours un défaut d'outillage, pas une découverte.

### 3. Séparer « ferrite est incomplet » de « ce n'est pas la même version »

Face à un écart, la question est toujours : **est-ce que ça casserait aussi
contre un vrai Elasticsearch de la version annoncée ?** Le probe 7.x tourne
contre trois serveurs (ferrite, ES 7.10.2, ES 8.15.0) exprès pour trancher ça.
Sur 11 échecs d'un client 7.x, 6 casseraient à l'identique contre un ES 8 : ce
sont des coûts de migration 7→8, pas des manques de ferrite.

### 4. Prendre les tests des autres

Le harnais maison teste ce à quoi on a pensé. La suite REST d'Elasticsearch
teste ce à quoi *Elastic* a pensé — et c'est elle qui a trouvé les deux vrais
manques (création d'index à l'écriture, routes sans index) qu'aucun test écrit
ici n'avait vus. Voir [`docs/conformance.md`](docs/conformance.md).

### 5. Vérifier une hypothèse sur une dépendance par un spike, pas par une lecture

`tests/spike_nested.rs` mesure deux propriétés de tantivy dont dépend tout le
support de `nested`. Elles ne sont pas documentées comme des garanties : le
spike les verrouille, et cassera bruyamment à la montée de version plutôt que le
jour où on s'appuiera dessus.

## Les outils, et ce que chacun répond

Tous depuis la racine. Les diffs exigent Docker (ce sont des outils de
développement, pas de CI).

| Commande | La question à laquelle elle répond |
|---|---|
| `./tests/compat/run.sh` | est-ce que le client officiel 8.x fait tout ce qu'on prétend ? (**68/68**) |
| `tests/compat/diff_relevance.py` | **les mêmes documents dans le même ordre** qu'ES ? (137/138, 0 écart réel) |
| `tests/compat/diff_against_es.py` | la même *forme* de réponse ? (39/40) |
| `tests/compat/diff_aggs.py` | les mêmes agrégations ? (34/34) |
| `tests/compat/diff_analyzers.py` | les mêmes tokens ? |
| `tests/compat/conformance_es.py` | que dit la suite de tests **d'Elastic** ? (44 réussis, 331 refus explicites, 171 échecs / 643) |
| `tests/compat/bench_vs_es.py` | mêmes résultats, **et à quel prix** ? (×3,5 en latence, ×9 en indexation) |
| `tests/compat/probe_es7.py` | un **client** 7.x peut-il se brancher ? |
| `tests/compat/diff_es7.py` | une **instance** 7.x peut-elle être reprise ? `--inventaire` liste ses types de champ |

Un chiffre qui bouge dans ce tableau se met à jour **dans la PR qui le fait
bouger**, pas après.

## Les décisions déjà prises (ne pas les rejouer sans raison)

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
  de tantivy (Snowball) n'est celui d'aucun des deux : `english` est identique à
  ES sur les 28 textes, `french` reste refusé pour sa seule liste de mots vides.
  Un analyzer n'est **jamais** livré sous le nom d'ES tant qu'il n'est pas
  mesuré identique. Les analyzers **sur mesure**
  (`settings.analysis`), eux, sont supportés : ils se composent de briques que
  ferrite reproduit à l'identique (`standard`, `lowercase`, `asciifolding`,
  `stop`).
- **La recherche refuse les motifs et les listes d'index**, là où `_refresh` et
  `_mapping` les acceptent. La distinction est volontaire : fusionner des
  résultats venus de mappings différents est précisément là où naissent les
  résultats faux.
- **Un champ inconnu dans une requête est une erreur, pas 0 résultat.** Idem
  pour un sous-champ de `nested` interrogé depuis la racine, là où ES rend 0 hit
  en silence. Les divergences assumées sont listées et justifiées dans
  [`docs/compat.md`](docs/compat.md).

## Les pièges rencontrés, pour ne pas les repayer

- **`search(doc_type=…)` d'un client 7.x écrit dans l'index.** L'URL
  `/{index}/_doc/_search` n'est plus une recherche en 8.x : c'est l'indexation
  d'un document dont l'`_id` est `_search`. Vérifié sur un vrai ES 8.15 **et**
  sur ferrite. À grepper avant toute migration.
- **Un pré-filtre doit être un sur-ensemble.** Le `nested` cassait sur les
  `must_not` : une négation évaluée à plat écarte un document dont une *autre*
  ligne satisfait la clause.
- **Un `[{…}]` accepté en silence.** `infer` rend `None` sur un objet comme sur
  un tableau d'objets ; seul le premier cas était testé, donc le second entrait
  dans `_source` sans entrer dans le mapping — invisible à la recherche.
- **Un conteneur ES qui vient de démarrer ment.** Un `diff_relevance` à 81/138
  s'est révélé être un ES 8.15 encore en train de se stabiliser. Re-mesurer
  avant de diagnostiquer.
- **Un `curl` de vérification qui n'utilise pas le même texte que le test ne
  vérifie rien.** Une chasse au bug d'analyzer s'est terminée sur un faux
  positif : `match edition` ne trouvait pas `l'édition` — ce que fait aussi ES,
  puisque `standard` garde l'élision. Comparer aux **deux** serveurs avant de
  conclure, y compris quand on croit tenir le coupable.

## Où va le projet

Le seul refus qui change encore les **résultats** plutôt que la forme :
l'analyzer **`french`**, et il ne tient plus qu'à sa liste de mots vides — le
stemmer et l'élision sont fidèles. La relever mot à mot contre un vrai ES est un
travail court et entièrement mesurable (`diff_analyzers.py`).

Ensuite, par ordre de gêne pour un projet réel : `scroll` / `helpers.scan`,
`rest_total_hits_as_int`, `_msearch`, `_stats`, les alias et les templates.

## Ton, et forme des livrables

Le dépôt est en français, y compris les messages de commit et la documentation.
Les commentaires de code évitent les accents (le reste non). Un commit explique
**pourquoi**, pas seulement quoi, et cite les mesures constatées plutôt que
« tests OK ». Une PR qui change un comportement met à jour `docs/compat.md` dans
la même PR.
