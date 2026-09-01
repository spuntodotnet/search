# dev-workflow — ferrite

> **Fichier lu par l'agent qui tourne sur le worker Spunto** dans le pipeline
> idée→prod. Le service `automations` ne fait que **créer** ce worker (carte
> Notion passée en `running`) et **l'arrêter** (au merge de la PR → `completed`).
> Tout le reste — implémenter, vérifier, faire avancer la carte, ouvrir la PR —
> c'est **toi**, l'agent, qui le pilotes en suivant ce fichier.

> Ce fichier dit **quoi lancer** et **quand**. [`../CLAUDE.md`](../CLAUDE.md) dit
> **comment penser** dans ce dépôt : la méthode, les décisions déjà prises, les
> pièges déjà rencontrés. À lire en premier dans une session neuve.

## Contexte de départ (déjà en place sur le worker)

- Branche dédiée déjà créée et checkout : `notion/{pageId}-{slug}`.
- `$NOTION_PAGE_ID` et `$NOTION_TOKEN` dans l'env, `gh` authentifié.
- Toolchain Rust stable installée (image devcontainer `rust`), `cargo` dans le
  `PATH`, Docker disponible (docker-in-docker).
- `cargo fetch` est passé au `postCreate` **si** un `Cargo.toml` existe déjà —
  sur un repo encore vide, c'est normal qu'il n'ait rien fait.

Le lien « App » du canal de la carte pointe le port **9200** du worker (le port
d'Elasticsearch, volontairement) : il ne répond qu'une fois le serveur lancé,
rien ne le démarre tout seul.

## Commandes

Toutes depuis la **racine** du repo.

| Commande | Ce qu'elle fait |
|---|---|
| `cargo run` | Lance le serveur sur `:9200` |
| `cargo build --release` | Build optimisé |
| `cargo test` | Tests unitaires + intégration (dont concurrence : ~6 s) |
| `cargo clippy --all-targets -- -D warnings` | Lint, **zéro warning toléré** |
| `cargo fmt --check` | Vérifie le formatage (`cargo fmt` pour corriger) |
| `./tests/compat/run.sh` | **Le harnais de compat** : compile, lance ferrite sur un port jetable, et l'exerce avec le client Elasticsearch officiel (critère d'acceptation + suite complète) |
| `python3 tests/compat/diff_against_es.py` | Compare la **forme** des réponses à celles d'un vrai ES (voir plus bas) |
| `python3 tests/compat/diff_relevance.py` | Compare les **résultats et leur ordre** à ceux d'un vrai ES, sur 600 documents |
| `python3 tests/compat/diff_aggs.py` | Compare les **agrégations** à celles d'un vrai ES, champ par champ |
| `python3 tests/compat/diff_analyzers.py` | Compare les **analyzers** à ceux d'un vrai ES : la suite ordonnée de tokens, **avec leurs positions et leurs offsets**. Les analyzers intégrés (les douze de langue compris), les déclarations de `ngram` / `edge_ngram` (tokenizer et filtre), les analyzers de Wagtail, et les classes de `token_chars` demandées caractère par caractère |
| `python3 tests/compat/sonde_langues.py [ferrite] [es]` | Compare les **analyzers de langue** sur un corpus qui n'est pas le nôtre : les vocabulaires du projet Snowball (BSD-3-Clause, licence vérifiée dans le clone, 563 000 mots), plus des phrases écrites pour les pièges de chaque langue. `--ecart` imprime **d'où vient l'écart** — la chaîne d'ES arrêtée après chaque étape, minuscules / élision / mots vides / normalisation / stemmer — et ne demande qu'ES, donc le tableau vaut même si on décide de refuser. `--mots-vides` régénère `src/mots_vides.rs` depuis le jar de Lucene du conteneur de référence, en le vérifiant contre lui dans les deux sens. `--calibrer` rejoue la batterie contre deux Elasticsearch |
| `python3 tests/compat/diff_datemath.py [ferrite] [es]` | Compare la résolution des **bornes de date** — `now`, `now-1d/d`, `2026-03-15\|\|+1M`, et l'arrondi d'une borne selon son côté — documents rendus et messages d'erreur compris |
| `python3 tests/compat/diff_highlight.py [ferrite] [es]` | Compare les **fragments surlignés** — pas leur nombre, leur contenu exact, balises comprises. Un fragment d'ES n'est ni « une phrase » ni « `fragment_size` caractères » : les phrases sont fusionnées vers l'avant tant que la longueur tient, puis re-coupées **au mot**. Le corpus est bâti pour les deux régimes, et fait varier `fragment_size` autour du seuil. `--calibrer` rejoue la batterie contre deux Elasticsearch : tant qu'elle n'y est pas à zéro, ce qu'elle dit de ferrite ne vaut rien |
| `python3 tests/compat/sonde_calendrier.py [ferrite] [es]` | Compare les seaux d'un **graphe temporel** — `calendar_interval`, `time_zone`, `offset`, `min_doc_count`, `extended_bounds`, `hard_bounds`, `keyed`, `format` — et `time_zone` sur la requête `range`. Le corpus est bâti pour les endroits où c'est difficile : les deux bascules de l'heure d'été **dans les deux sens**, un 29 février, une zone dont **minuit n'existe pas** ce jour-là (`America/Santiago`), une heure d'été d'une demi-heure (`Australia/Lord_Howe`), et des documents posés exactement sur une frontière de seau. Ce qui est comparé est le **bloc entier**, seau par seau : un `key_as_string` faux (`Z` là où ES écrit `+01:00`) est un graphe dont l'axe ment sans qu'aucun compte ne bouge. `--calibrer` rejoue la batterie contre deux Elasticsearch |
| `python3 tests/compat/genere_fuseaux.py [--verifie\|--grille]` | Régénère [`src/tzdata.bin`](../src/tzdata.bin) — la table des fuseaux — depuis le **tzdb du JDK que le conteneur de référence embarque**, c'est-à-dire les règles qu'Elasticsearch applique lui-même (son image n'a pas de `/usr/share/zoneinfo`). `--verifie` est le cliquet : il redumpe et compare octet par octet. `--grille` écrit [`tests/donnees/arrondis.jsonl`](../tests/donnees/arrondis.jsonl), 25 914 arrondis calculés par **la classe `org.elasticsearch.common.Rounding` d'ES elle-même**, exécutée dans le conteneur avec ses jars au classpath — l'oracle de `tests/arrondi_vs_es.rs`, qui tourne dans `cargo test` sans Docker |
| `python3 tests/compat/sonde_score.py [ferrite] [es]` | Compare le **`_score` lui-même**, pas seulement l'ordre : `function_score` et `boosting` n'existent que pour produire un nombre, et un ordre juste avec des scores faux serait vert partout ailleurs. 197 questions posées aux deux serveurs, comparées sur le score de chaque hit, `max_score`, le total et l'ordre. La plupart partent d'une requête dont le score de base est **exact des deux côtés** (une somme de `constant_score`) : l'égalité y est exigée au bit près. Celles marquées `[bm25]` partent d'un vrai `match`, dont tantivy et Lucene ne calculent pas le dernier bit pareil — leur tolérance n'est pas choisie, c'est l'écart **mesuré sur la requête nue** plus trois arrondis de `float`. `--calibrer` rejoue la batterie contre deux Elasticsearch |
| `python3 tests/compat/genere_scoring.py [--verifie]` | Régénère [`tests/donnees/scoring.jsonl`](../tests/donnees/scoring.jsonl) — la grille de scoring — en exécutant **les classes d'Elasticsearch elles-mêmes** dans le conteneur de référence : `GaussDecayFunctionBuilder$GaussScoreFunction` et ses deux sœurs, `FieldValueFactorFunction$Modifier`, `CombineFunction`. 58 476 points, rejoués par `tests/scoring_vs_es.rs` dans `cargo test` **sans Docker**. `--verifie` redump et compare — c'est le cliquet |
| `python3 tests/compat/diff_motifs.py` | Compare les **motifs** — `regexp`, `wildcard`, `prefix`, `match_phrase_prefix` — sur un corpus fait pour les pièges de la syntaxe de Lucene (casse, accents, caractères spéciaux) |
| `python3 tests/compat/sonde_msm.py [ferrite] [es]` | Compare les **notations de `minimum_should_match`** — entier, pourcentage, formes négatives, conditions `3<90%` — sur un `bool` et sous un `nested`, en posant à chaque serveur des requêtes dont le compte de résultats dit directement quel minimum a été appliqué |
| `python3 tests/compat/sonde_fields.py [ferrite] [es]` | Compare **ce que la réponse transporte** — `fields`, `docvalue_fields`, `stored_fields` — sur le **hit entier** : le bloc `fields` clé par clé, la présence de `_source`, la présence de `_id`. Les trois ne lisent pas au même endroit (le `_source`, les colonnes, les champs déclarés `store: true`), et c'est là qu'ils divergent — y compris sur la cible d'un `copy_to`, qui n'est nulle part dans le `_source`. Refuse de tourner si elle ne trouve pas les **deux** serveurs : une sonde différentielle qui ne compare rien ne doit pas rendre de verdict |
| `python3 tests/compat/sonde_par_requete.py [ferrite] [es]` | Compare `_delete_by_query` et `_update_by_query` : les **compteurs** de la réponse (`total`, `deleted` / `updated`, `batches`, `version_conflicts`, `failures[]`) **et l'état laissé derrière** — documents restants, `_version`, `_source`. Une commande qui rend les bons compteurs en supprimant les mauvais documents serait verte sur les compteurs seuls. Les conflits sont provoqués pour de vrai, par une écriture non rafraîchie. Refuse de tourner si elle ne trouve pas les **deux** serveurs |
| `python3 tests/compat/sonde_tri.py [ferrite] [es]` | Compare les trois paramètres d'une clé de `sort` — `missing`, `mode`, `unmapped_type` — sur **l'ordre complet des documents, ex æquo compris, plus le tableau `sort` de chaque hit**. C'est cette seconde moitié qui compte : un `mode` faux change l'ordre sans changer le total, et une sentinelle fausse ne se voit que dans le tableau `sort`. C'est elle qui a montré que `_first` / `_last` sont **sensibles à la casse**, qu'une substitution de date est un nombre de millisecondes, que `mode: sum` **déborde en silence** comme un `long` de Java, et que deux index dont le tri ne tombe pas dans la même famille (`LONG` / `FLOAT` / `DOUBLE` / `STRING`) font échouer la recherche entière. `--calibrer` rejoue la batterie contre deux Elasticsearch — elle **écrit**, donc elle ne peut pas s'étalonner contre un seul. Refuse de tourner si elle ne trouve pas ses deux cibles |
| `python3 tests/compat/sonde_facettes.py [ferrite] [es]` | Compare ce qui sépare un `terms` d'une **facette** : `include` / `exclude` (expression régulière de Lucene ou liste exacte), l'**ordre par sous-agrégation**, et leur cohabitation avec `missing`, `size`, `shard_size` et les deux compteurs. Elle compare le **bloc `terms` entier** — seaux dans leur ordre, valeurs des sous-agrégations, `sum_other_doc_count` et `doc_count_error_upper_bound` — parce qu'un ordre faux garde exactement les mêmes seaux et que les deux compteurs se calculent **après** filtrage. Deux corpus : huit catégories pour la syntaxe et les refus, 800 catégories sur 6 000 documents pour ce qui ne se voit qu'à cette taille (une métrique **vide**, et la bascule de `doc_count_error_upper_bound` à `-1`). `--calibrer` rejoue la batterie contre deux Elasticsearch. Refuse de tourner sans ses deux cibles |
| `python3 tests/compat/sonde_alias.py [ferrite] [es]` | Compare les **expressions de noms d'alias** de `GET /_alias/{nom}` — listes, jokers, exclusions, `_all` — statut, corps et message du 404 compris. C'est elle qui a montré que le tiret n'exclut qu'à partir du deuxième terme, et que la présence d'un joker change la règle du 404 |
| `python3 tests/compat/sonde_ecriture_alias.py [ferrite] [es]` | Compare l'**écriture** d'un alias, là où `sonde_alias.py` en compare la lecture : les **sept URL** de `put_alias` (le nom de l'alias, celui de l'index, ou les deux, peuvent venir du corps — et le corps remplace le chemin), `must_exist` sur un `remove`, et les deux règles de 404 qui ne sont pas la même (`must_exist: true` se vérifie **par index visé**, le 404 par défaut est **global**). Chaque cas remet les deux serveurs dans le même état, puis compare le statut, le message **et l'état laissé derrière** — une commande qui rend 200 en posant l'alias sur un autre index serait verte sur le statut seul. `--calibrer` rejoue la batterie contre deux Elasticsearch : elle **écrit**, donc elle ne peut pas s'étalonner contre un seul |
| `python3 tests/compat/sonde_index_false.py [ferrite] [es]` | Compare ce que devient un champ **`index: false`** — type par type (`keyword`, `text`, `long`, `double`, `date`, `boolean`) et opération par opération (`term`, `terms`, `range`, `match`, la phrase, les motifs, `exists`, le tri, les agrégations, `fields`, `docvalue_fields`, le surlignage, `_field_caps`, l'aller-retour du mapping). Le paramètre n'est pas « à accepter » : c'est une famille de conséquences, et aucune ne se devine — un champ non indexé **garde sa colonne**, et ES y retombe. `--calibrer` rejoue la batterie contre deux Elasticsearch. Refuse de tourner sans ses deux cibles |
| `python3 tests/compat/sonde_vide.py [ferrite] [es]` | Compare ce que rendent les deux serveurs quand ils n'ont **aucun index** — l'état que le harnais n'exerçait pas, et la raison pour laquelle le seul échec silencieux du projet y a vécu si longtemps. Sépare ce que les deux doivent rendre à l'octet près (requêtes valides, erreurs de lecture du corps) de ce que ferrite refuse alors qu'ES sait le faire. Refuse de tourner si un serveur n'est pas vide |
| `python3 tests/compat/diff_multi_index.py [ferrite] [es]` | Compare la **résolution des noms d'index** — listes, motifs, `_all`, exclusions, alias — et la **fusion multi-index** des résultats et des agrégations. `--calibrer [es_a] [es_b]` fait tourner la même batterie contre deux Elasticsearch, parce qu'une batterie qui modifie l'état du serveur ne peut pas s'étalonner contre un seul |
| `python3 tests/compat/probe_es7.py [URL]` | Exerce le serveur avec le client officiel **7.x** (code écrit pour un ES 7) : ce qu'un projet resté en 7.10.2 peut brancher tel quel — voir [`compat-es7.md`](compat-es7.md) |
| `python3 tests/compat/diff_es7.py [ferrite] [es7]` | Rejoue les **index, documents et requêtes** d'une instance **7.x** sur ferrite : ce qui s'héberge, ce qui se transfère, ce qui rend les mêmes résultats. Ne lit que l'instance (`--sans-ecriture` pour n'y rien écrire du tout ; `--inventaire URL` pour se contenter de lister les types de champ qu'elle utilise) |
| `python3 tests/compat/bench_vs_es.py [ferrite] [es]` | **Le banc rapide** : mêmes documents et mêmes requêtes des deux côtés, puis indexation, latence, débit — et le compte des requêtes qui rendent le même résultat. 600 documents et 138 requêtes **écrites ici** : c'est un garde-fou de développement, pas de quoi étayer un chiffre publié (voir la ligne suivante). Sans client Elasticsearch (HTTP brut), donc utilisable contre un ES 7.x comme 8.x |
| `python3 tests/compat/bench_echelle.py [ferrite] [es] --docs N` | **Le banc à l'échelle**, celui dont sortent les chiffres publiés : le corpus **et** les requêtes viennent de la track Rally `geonames` d'Elastic (Apache-2.0, révision figée, taille du corpus vérifiée à l'octet près), à 500 000 et 2 000 000 de documents. Mesure indexation, taille sur disque, RSS, latence (médiane / p95 / p99) et débit, **publie ce que ferrite perd**, et rattache chaque requête refusée à une capacité de `compat.yaml`. `--inventaire` imprime ce que la track demande et les six écarts appliqués, sans rien mesurer. Voir [`bench.md`](bench.md) |
| `python3 tests/compat/sonde_sous_aggs.py [ferrite] [es] [--seuil]` | Une **sous-agrégation** voit-elle tous les documents de son bucket ? 46 combinaisons parent × sous-agrégation posées aux deux serveurs sur 50 000 documents déséquilibrés : **46/46** avec l'épingle de tantivy, **32/46** sans — c'est le second chiffre qui prouve que le premier mesure quelque chose. `--seuil` rejoue les deux bornes du défaut (2 047 juste / 2 048 faux ; 204 documents perdus / 205 gardés dans un bucket sur 5). Refuse de tourner sans ses deux cibles |
| `python3 tests/compat/verifie_tantivy.py` | **Qu'est-ce que l'épingle de `Cargo.toml` contient vraiment ?** ferrite ne prend pas tantivy sur crates.io mais sur un fork (le tag 0.26.1 plus le correctif d'amont des sous-agrégations). Ce script télécharge les **9 crates publiées** que l'épingle remplace, extrait l'arbre du fork au commit épinglé, et compare fichier par fichier : le seul écart toléré est écrit en dur. Tourne en CI. Voir [`tantivy-patch.md`](tantivy-patch.md) |
| `python3 tests/compat/fuzz_vs_es.py [ferrite] [es] [--cas N]` | **Le fuzzer différentiel** : un mapping, des documents et des requêtes tirés au sort **dans le périmètre déclaré** (`compat.yaml` dit ce qui est jouable), envoyés aux deux serveurs, réponses comparées. `--calibrer [es_a] [es_b]` fait tourner la même batterie contre deux Elasticsearch — tant qu'elle n'y est pas à zéro, ce qu'il dit de ferrite ne vaut rien. `--rejouer <graine>` rejoue un cas en détail, `--couverture` imprime ce qu'il **ne** fuzze pas. Voir [`fuzz.md`](fuzz.md) |
| `python3 tests/compat/sonde_fuzz.py [ferrite] [es]` | Les écarts que le fuzzing a trouvés, **figés** : chacun réduit au plus petit mapping qui le montre, avec la phrase de ce qui était faux. Une graine ne se rejoue qu'à générateur constant ; un cas écrit, si |
| `python3 tests/compat/appli_reelle.py [gitea\|wagtail] [--json docs/application.json]` | **Une vraie application, non modifiée.** Clone la cible à une révision figée, refuse de conclure si un fichier suivi a bougé, lance sa **propre** suite d'intégration contre un vrai Elasticsearch **puis** contre ferrite, et relève tout le trafic HTTP au passage (chaque refus est rattaché à une capacité de `compat.yaml`). `--liste` imprime les cibles retenues **et les candidats écartés, avec la raison**. Voir [`application.md`](application.md) |
| `python3 tests/compat/tests_clients.py [python\|go\|javascript] [--cycle] [--json docs/clients.json]` | **La suite de tests d'un client officiel, jouée par son propre lanceur.** Clone le client à une révision figée, **vérifie sa licence dans le clone** (Apache-2.0 pour les trois), lance sa suite contre un vrai ES 8.15 **puis** contre ferrite, et rattache chaque refus à une capacité de `compat.yaml`. `--liste` imprime les clients retenus **et les écartés, avec la raison mesurée**. `--cycle` ne joue que la batterie « cycle de vie du client » — découverte de version, en-tête `X-elastic-product`, compression, sniffing (ou son refus propre), erreurs typées, helpers — écrite ici mais exécutée par le client **publié**. Le mouchard écoute sur le **9200** (des cas l'écrivent en dur), donc ferrite doit être ailleurs. Voir [`clients.md`](clients.md) |
| `python3 tests/compat/genere_compat.py [--verifie]` | Regénère [`docs/compat.md`](compat.md) et [`docs/compat.json`](compat.json) depuis [`compat.yaml`](../compat.yaml), la **source** du périmètre déclaré (le texte long reste écrit à la main dans [`compat.gabarit.md`](compat.gabarit.md)). `--verifie` est ce que lance la CI : elle échoue si le fichier commité diffère de sa source |
| `python3 tests/compat/recolte_usage.py` | Constitue le **corpus de vraies requêtes** ([`tests/compat/usage/corpus.jsonl`](../tests/compat/usage/corpus.jsonl)) depuis quatre sources publiques et citables : la doc de référence d'ES 8.15, les tracks Rally d'Elastic, les tests et exemples des clients officiels, et la recherche de code de GitHub. Chaque requête porte l'URL exacte d'où elle vient ; les dépôts sont clonés à la demande dans `.corpus-usage/` (ignoré par git) |
| `python3 tests/compat/ponderation.py [--json docs/usage.json] [--rejoue ferrite es] [--poids] [--verifie]` | **Ce que ce corpus réclame, et ce que ferrite en sert entièrement.** Croise chaque requête avec `compat.yaml` (une seule clause refusée fait tomber la requête), et `--rejoue` pose la même requête à ferrite et à un vrai ES 8.15 pour étalonner ce croisement. `--poids` écrit les `poids` de `compat.yaml`, `--verifie` est le cliquet de la CI. L'étude : [`usage.md`](usage.md) |
| `python3 tests/compat/perimetre.py [api] [message]` | Rattache un cas échoué de la suite de conformance à une capacité de `compat.yaml` — et donc dit si c'est une **régression** ou un **coût de périmètre**. Sans argument, imprime l'index tel qu'il est lu |
| `python3 tests/compat/conformance_es.py [URL] [--source elasticsearch\|opensearch] [--json <fichier>] [--diff <ancien.json>] [--divergences <rapport.json>] [--etat]` | Rejoue la **suite de conformance REST d'un autre moteur** — celle d'Elasticsearch (7.10.2, Apache 2.0, 107 domaines) ou celle d'**OpenSearch** (2.19.1, Apache 2.0, 112 domaines), téléchargées à la demande dans `.es-rest-spec/` et `.opensearch-rest-spec/`. Sans liste blanche : le tri se fait dans le rapport, pas par omission. Les cas ne viennent pas de nous, c'est ce qui attrape ce qu'on ne sait pas qu'on ignore — et deux équipes valent mieux qu'une, la suite d'Elastic étant figée en 2020. `--json` écrit le rapport machine (commité, source de tous les chiffres publiés) ; `--diff` dit ce qui a bougé depuis un rapport et fait du code de sortie un **cliquet** (celui du job CI `conformance`) ; `--divergences` range à part les cas qu'un **vrai** moteur de référence échoue lui aussi, donc qui ne mesurent pas ferrite — mesuré, pas décidé ; `--etat` vérifie entre deux cas qu'aucun index, alias, template ni réglage de cluster n'est **apparu** depuis l'état de départ de la cible (pas depuis le vide : un vrai ES réinstalle ses templates x-pack) et **arrête la campagne** au premier écart — +27 % de durée, payés par la CI |
| `./tests/compat/measure_container.sh --json docs/container.json` | **La campagne du conteneur** : mesure les **deux** images que le README compare — ferrite et l'Elasticsearch de référence — dans la même campagne, sur la même machine, avec le même outil, et écrit le rapport machine [`docs/container.json`](container.json) (commité). Une entrée par image (référence, tag, digest du manifeste, arguments du `docker run`), et pour chaque valeur **sa définition en une phrase** — c'est elle qui rend le nombre lisible. Le démarrage est la médiane de 5 tours, tous publiés. L'image de ferrite qu'elle mesure est celle que `Cargo.toml` déclare (`ferrite:{version}`) : absente, elle est refusée en nommant le `docker build` qui la produit ; présente mais servie par un binaire d'une autre version (`build_hash`), elle est refusée aussi. Elle est donc à relancer **dans la PR qui bump la version** — voir [Publier une version](#publier-une-version) |
| `python3 tests/compat/chiffres_conteneur.py --injecte \| --verifie` | **Le cliquet** : réécrit (ou vérifie) depuis `docs/container.json` les blocs `<!-- chiffres-conteneur:… -->` du README et de [`bench.md`](bench.md), et les phrases qui citent un de ces nombres. `--verifie` est ce que lance la CI, comme `genere_compat.py --verifie` : plus aucun chiffre de conteneur ne se saisit à la main. Un marqueur ou un motif introuvable est une **erreur** — une vérification qui ne trouve rien à comparer ne rend pas de verdict vert. Il compare aussi la version : celle de `Cargo.toml`, celle du rapport, le tag de l'image mesurée et le `build_hash` du binaire qui a répondu. C'est lui qui a bloqué la release 0.8.0, et c'est ce qu'on lui demande |
| `./tests/compat/measure_container.sh [tag] [-- args docker run]` | Ne construit rien, mesure une image déjà buildée (par défaut `ferrite:{la version de Cargo.toml}`) : **les trois tailles** (compressée telle qu'un registre la sert, décompressée, binaire seul), le RSS au repos et le temps de démarrage. Ne lit aucun champ dont la définition change avec la version de Docker (`{{.Size}}` valait la taille décompressée jusqu'à Docker 28 et vaut la compressée depuis la 29) : il demande l'image par `docker save` et compte les octets. `--tailles IMAGE…` ne mesure que les tailles, de n'importe quelle image — c'est ainsi qu'Elasticsearch est mesuré avec la même définition. Si une couche de l'archive n'est **pas** compressée (le `docker save` du magasin d'images classique), la taille qu'un registre servirait n'est pas déductible : elle est refusée (`NON MESURABLE`, code de retour non nul) et pas remplacée par un nombre plausible. `IMAGE_TAR=…` fait alors lire l'artefact OCI de `docker buildx build --output type=oci` — ce qu'un `docker push` enverrait, et ce que fait la CI |
| `docker build -t ferrite .` | Image minimale (`scratch` + binaire statique musl) |

Le harnais de compat installe le client officiel dans un venv (`.venv-compat/`)
s'il n'est pas déjà disponible. Il accepte `FERRITE_PORT` (port d'écoute) et
`FERRITE_URL` (viser un serveur déjà lancé, sans rien compiler).

Il **refuse** de démarrer si un serveur répond déjà sur son port : son `bind`
échouerait sans bruit et il exercerait cet autre serveur, en y laissant index,
templates et réglages de cluster. C'est ce qui a fait dérailler une campagne de
fuzzing entière (400 cas partis en divergences de mapping qui n'existaient pas)
avant que le garde-fou n'existe. Viser un serveur déjà lancé se demande
explicitement, avec `FERRITE_URL`.

Ce fichier doit toujours décrire les commandes réelles du repo : une PR qui
change ces commandes met ce tableau à jour dans la même PR.

### Comparer à un vrai Elasticsearch

Le moyen le plus rapide de trouver un écart, c'est de faire répondre les deux
serveurs à la même question :

```bash
docker run -d --name es-ref -p 9201:9200 \
  -e discovery.type=single-node -e xpack.security.enabled=false \
  -e ES_JAVA_OPTS="-Xms512m -Xmx512m" \
  docker.elastic.co/elasticsearch/elasticsearch:8.15.0

cargo run &                                     # ferrite sur :9200
python3 tests/compat/diff_against_es.py         # la forme des réponses
python3 tests/compat/diff_relevance.py          # les résultats et leur ordre
```

Deux comparateurs, qui ne cherchent pas la même chose :

| Script | Ce qu'il compare |
|---|---|
| `tests/compat/diff_against_es.py` | la **forme** des réponses — champ par champ, sur 46 appels, après neutralisation des valeurs qui ne peuvent pas coïncider (durées, uuid, scores, `_scroll_id`). Le `scroll` y est comparé sur son **déroulé complet** : mêmes pages, mêmes documents, même fin |
| `tests/compat/diff_relevance.py` | la **pertinence** — même corpus de 600 documents des deux côtés, ~205 requêtes générées, et pour chacune : même total, mêmes documents, **même ordre** |
| `tests/compat/diff_aggs.py` | les **agrégations** — 73 requêtes, comparaison du JSON champ par champ, clés comprises (dont 11 sur l'agrégation `filter`, que ferrite exécute lui-même, 8 sur ce qu'un bucket **vide** doit porter, et 20 sur `include` / `exclude` et l'ordre par sous-agrégation) |
| `tests/compat/diff_analyzers.py` | les **analyzers** — 51 batteries × 217 textes, comparées sur `(terme, offsets, position)`. C'est elle qui a montré que les offsets d'`_analyze` se comptent en unités UTF-16 et non en octets, et qu'un n-gramme produit par un **filtre** se pose à la position de son mot, pas à la suivante |
| `tests/compat/sonde_langues.py` | les **analyzers de langue** — ceux que `diff_analyzers.py` ne peut pas juger, parce qu'un stemmer a des dizaines de branches et que 217 textes écrits ici n'en visitent aucune. Les vocabulaires du projet **Snowball** (BSD-3-Clause, 20 913 à 96 325 mots par langue, 563 000 en tout) posés aux deux serveurs. C'est elle qui a montré que le Snowball de tantivy **est** celui de Lucene sur huit langues, que `DutchAnalyzer` impose un dictionnaire de quatre mots avant son stemmer, et que `articles_case` du filtre `elision` est un `ignoreCase` — donc l'inverse de ce que son nom dit |
| `tests/compat/diff_datemath.py` | les **bornes de date** — 276 bornes posées aux deux serveurs sur un corpus d'instants placés sur les bords (minuit, dernière milliseconde d'un jour, d'un mois, d'une année) : une milliseconde d'arrondi de travers change la réponse. Le même fichier lancé contre le ferrite d'avant rend 45/276 — c'est ce qui prouve qu'il mesure quelque chose |
| `tests/compat/sonde_calendrier.py` | le **calendrier** — 233 questions posées aux deux serveurs, comparées sur le bloc de seaux entier. C'est elle qui a montré qu'un `keyed` garde son `key_as_string` **dans** le seau (contrairement au `key` d'un `range` keyed), que `hard_bounds` exclut sa borne haute après l'avoir arrondie, et qu'une borne de `hard_bounds` ou d'`extended_bounds` se lit **dans le fuseau** |
| `tests/compat/sonde_score.py` | le **score**, pas l'ordre — 197 questions posées aux deux serveurs. C'est elle qui a montré que `min_score` compare le score **après** le `boost` de la clause, qu'une fonction unique sans `filter` fait **ignorer** `score_mode`, et qu'un document sans valeur a une distance **nulle** (donc un score de décroissance de 1.0) là où un `field_value_factor` sans `missing` fait échouer la recherche entière |
| `tests/compat/diff_motifs.py` | les **motifs** — 110 motifs posés aux deux serveurs sur un corpus construit pour eux : la syntaxe de `regexp` est celle de Lucene, pas celle du moteur qui l'exécute, et les deux divergent là où personne ne regarde (`\d`, `^`, `@`, `case_insensitive`, et le `|` sans branche gauche — qui est un caractère **littéral** chez Lucene, pas une alternation vide) |
| `tests/compat/diff_highlight.py` | les **fragments surlignés** — 233 questions posées aux deux serveurs, comparées caractère par caractère. C'est elle qui a montré qu'un point suivi d'une **minuscule** ne termine pas une phrase (donc que « zzz cible. aaa. bbb cible cible. » n'en fait qu'une), et que le fragment se centre sur le **milieu** de la correspondance et non sur son début |
| `tests/compat/sonde_tri.py` | les trois paramètres d'une clé de **`sort`** — 224 questions posées aux deux serveurs, comparées sur l'ordre complet **et** le tableau `sort` de chaque hit. C'est elle qui a montré que `missing: "_FIRST"` n'est pas un mot-clé mais une valeur (donc 400 sur un `long`), que `mode: avg` sur des entiers arrondit par le `Math.round` de Java, et que `float` et `double` ne trient pas dans la même famille |
| `tests/compat/sonde_facettes.py` | ce qui sépare un `terms` d'une **facette** — 170 questions posées aux deux serveurs, comparées sur le bloc `terms` entier. C'est elle qui a montré qu'un seau dont la métrique n'a **aucune** valeur ne se classe pas au même endroit selon la métrique (`NaN` sous un `avg`, `+∞` sous un `min`, `-∞` sous un `max`), que `doc_count_error_upper_bound` bascule à `-1` sur un ordre par sous-agrégation exactement comme sur `_count asc`, et que le seau de `missing` disparaît dès qu'un filtre de termes est posé |
| `tests/compat/sonde_msm.py` | les notations de **`minimum_should_match`** — 53 requêtes dont le compte de résultats dit quel minimum a été appliqué, sur un `bool` et sous un `nested`. C'est elle qui a montré que l'arrondi d'ES est une troncature vers zéro, qu'un minimum supérieur au nombre de clauses n'est pas plafonné, et que le séparateur de `2<-25% 9<-3` est l'espace |
| `tests/compat/diff_multi_index.py` | les **expressions d'index** et le multi-index — `es.search(index=["a","b"])`, `logs-*`, `_all`, exclusions, alias, `is_write_index`, purge en `DELETE /logs-2026.07.*` : total, ordre des `(_index, _id)`, `_shards`, agrégations fusionnées, statut et type d'erreur |
| `tests/compat/probe_es7.py` | ce qu'un **client 7.x** obtient — le même fichier se lance contre ferrite, contre un `elasticsearch:7.10.2` et contre un `elasticsearch:8.15.0`, ce qui sépare « ferrite est incomplet » de « la 8 a supprimé ça » |
| `tests/compat/bench_vs_es.py` | le **prix** de ces résultats — indexation, latence médiane et p95, débit à 8 requêtes en vol, mesurés sur les deux serveurs avec la même batterie. Sur 600 documents : de quoi voir une régression pendant qu'on code, pas de quoi publier |
| `tests/compat/bench_echelle.py` | le même prix, mais **à l'échelle et sur un corpus qui n'est pas le nôtre** — la track Rally `geonames` d'Elastic, 500 000 puis 2 000 000 de documents, ses 31 requêtes. C'est elle qui a montré que l'avantage de ferrite sur un `term` grandit avec l'échelle (×1,5 puis ×1,7) et que son tri s'effondre (jusqu'à ×290), et c'est en la lançant qu'on a trouvé qu'une sous-agrégation perdait des documents |
| `tests/compat/sonde_sous_aggs.py` | ce que le banc à l'échelle a trouvé, **figé** hors d'un corpus de 500 000 documents : la sous-agrégation d'un bucket rare perdait des documents au-delà de 2 048 par segment. C'est elle qui a mesuré les deux bornes sur lesquelles la décision s'est prise — et elle reste là pour que la correction ne se défasse pas en silence |
| `tests/compat/sonde_fields.py` | ce que la réponse **transporte** — 110 questions posées aux deux serveurs. C'est elle qui a montré qu'un `keyword` ressort trié et dédoublonné d'une colonne (`docvalue_fields`) mais dans l'ordre du document depuis le `_source` (`fields`), qu'un `float` en colonne vaut `0.10000000149011612` là où le `_source` porte `0.1`, et qu'une valeur écartée par `ignore_above` ne sort pas dans `fields` mais dans `ignored_field_values` — et que l'ordre des valeurs qu'un `copy_to` dépose dans sa cible est celui d'un `HashSet` de Java, donc pas un ordre |
| `tests/compat/sonde_par_requete.py` | **modifier ou purger par requête** — 74 commandes posées aux deux serveurs, dont la moitié sont des refus attendus. C'est elle qui a montré que `total` vaut `min(correspondants, max_docs)` **même quand la commande s'interrompt**, que `conflicts=abort` s'arrête à la fin du **lot** fautif et pas au document, et que `refresh=wait_for` y est refusé alors que `_doc` l'accepte |
| `tests/compat/sonde_alias.py` | les **expressions de noms d'alias** — 21 expressions posées aux deux serveurs, choisies pour séparer les lectures possibles de la règle. C'est elle qui a montré que `test_alias_1,-test` rend 404 là où `test_blias_2,test_alias*,-test_alias_1` rend 200 : la même exclusion d'un alias qui existe, et c'est le joker qui les sépare |
| `tests/compat/sonde_ecriture_alias.py` | l'**écriture** d'un alias — 62 commandes posées aux deux serveurs, comparées sur le statut, le message et l'état laissé derrière. C'est elle qui a montré que `must_exist: true` se vérifie **par index visé** (un `remove` sur `logs-*` échoue dès qu'un seul des index ne porte pas l'alias) alors que le 404 par défaut est **global**, et que le corps de `PUT /_alias` ne lit que `index` et `alias` au singulier — d'une liste JSON, ES ne garde que le dernier élément, en 200 |
| `tests/compat/sonde_index_false.py` | **`index: false`** — 244 questions posées aux deux serveurs. C'est elle qui a montré qu'un `keyword` non indexé reste cherchable (par sa colonne, à score **constant**) alors qu'un `text` ne l'est plus du tout, que le refus d'une **phrase** dépend du nombre de termes, et que le surlignage n'y marque que la famille des **automates** — un `term` trouve le document sans le marquer |
| `tests/compat/sonde_vide.py` | le **serveur vide** — 28 questions posées aux deux, dont le corps entier se compare sur un 200 : `_shards` à zéro, `max_score` à `0.0` (et non `null`), pas de section `aggregations`. C'est elle qui a montré qu'ES refuse sans index ce qu'il refuse avec, et seulement ça |
| `tests/compat/conformance_es.py` | les tests d'**Elastic** et ceux d'**OpenSearch**, pas les nôtres — deux suites REST écrites par deux équipes, 107 et 112 domaines. Chacune se valide en la lançant contre un vrai serveur du moteur qui l'a écrite, où elle doit être quasi tout vert ([`conformance-es7102.json`](conformance-es7102.json), [`conformance-opensearch-os2191.json`](conformance-opensearch-os2191.json)) — le conteneur doit alors porter les réglages du cluster de test (`node.attr.testattr`, `path.repo`), voir [`conformance.md`](conformance.md). Ses mesures sur ferrite sont [`conformance.json`](conformance.json) et [`conformance-opensearch.json`](conformance-opensearch.json) : totaux, taux, exclusions comptées, détail par cas |
| `tests/compat/diff_es7.py` | ce qu'une **instance 7.x** peut céder à ferrite — ses index (rejoués), ses documents (`scan` + `bulk`), ses requêtes (même corpus, même ordre attendu) |
| `tests/compat/ponderation.py --rejoue` | ce que **5 311 vraies requêtes** deviennent : la même posée aux deux serveurs sur un index vide, mais dont le mapping est déduit de la requête elle-même (sans quoi toute agrégation serait refusée faute de champ). Ne compare qu'« accepté / refusé » — ce qu'ES refuse aussi sort du dénominateur. C'est lui qui a montré qu'un champ non mappé avalait le refus de `range`, `term`, `terms` et `regexp` |
| `tests/compat/fuzz_vs_es.py` | ce à quoi **personne n'a pensé** — mapping, documents et requêtes tirés au sort dans le périmètre déclaré, comparés champ par champ. Il s'étalonne contre deux Elasticsearch avant de servir, et chaque divergence qu'il laisse passer porte un prédicat écrit, pas un code d'état toléré en bloc |
| `tests/compat/sonde_fuzz.py` | les écarts déjà trouvés par le fuzzing, **figés** — ils ne dépendent plus d'une graine |
| `tests/compat/tests_clients.py` | ce que les lignes précédentes ne mesurent pas non plus : elles passent **par** un client officiel, mais c'est nous qui écrivons ce qu'on lui demande. Ici ce sont **les tests du client, joués par son lanceur** — donc la couche que personne d'autre n'exerce : poignée de main, en-tête de produit, compression, sniffing, carte statut → exception, helpers. C'est elle qui a trouvé qu'un `_id` numérique dans un `_bulk` était traité comme un `_id` absent, en 201 et sans un mot |
| `tests/compat/appli_reelle.py` | ce qu'aucune des lignes précédentes ne mesure — **un logiciel écrit par d'autres**, cloné tel quel, qui lance sa propre suite contre les deux serveurs. C'est lui qui a trouvé qu'un mapping écrit par un générateur (`"index": true` sur chaque champ) bloquait une application entière au démarrage, là où le corpus d'usage et la suite d'Elastic ne bougeaient ni l'un ni l'autre — et, trois cartes plus tard, que `{"bool": {"mustNot": …}}` était le dernier refus de trop entre Wagtail et ses 83 tests |

Le second est celui qui compte pour un moteur de recherche : l'ordre des
résultats est précisément ce qu'un test écrit à la main ne sait pas vérifier,
puisqu'on l'écrirait avec la même idée fausse que le code. Il signale un écart
d'ordre sauf s'il ne porte que sur des documents qu'Elasticsearch lui-même
classe ex æquo.

Le corpus vit dans `tests/compat/corpus.py` et est généré avec une graine fixe :
un écart constaté est toujours reproductible.

Ce sont des outils de développement, pas des tests de CI : ils exigent Docker.

Une exception : `conformance_es.py` n'a besoin que de ferrite et des suites
(téléchargées, mises en cache). Le job CI `conformance` le lance donc contre un
ferrite fraîchement compilé, **une fois par source**, et compare aux rapports
commités `docs/conformance.json` et `docs/conformance-opensearch.json` — c'est un
**cliquet** : il échoue si le nombre d'échecs augmente, ou si un cas passe de
réussi à échec. Une PR qui fait bouger la mesure régénère le rapport dans la même
PR (`--json …`). La référence des divergences
(`docs/conformance-opensearch-es8150.json`) est elle aussi un fichier commité :
la CI n'a aucun conteneur à démarrer.

## La règle qui prime sur tout : la compatibilité se prouve, elle ne se déclare pas

Le produit, c'est « le code client existant ne change pas ». Donc **la seule
preuve valable qu'une fonctionnalité marche, c'est un vrai client Elasticsearch
officiel qui l'exerce contre `ferrite`** — pas un `curl` bien choisi, pas un
test unitaire sur le parseur de DSL.

Il y a un client Python installable dans le worker (`pip install elasticsearch`,
ou `uv`). Le harnais de compat vit dans `tests/compat/` : chaque fonctionnalité
livrée y ajoute un scénario qui passe par le client, pas par HTTP brut.

Deux corollaires non négociables :

- **Jamais d'échec silencieux.** Une clause de DSL, un paramètre de mapping ou
  une route non supportés doivent renvoyer une erreur explicite au format
  d'erreur d'Elasticsearch. Renvoyer des résultats faux parce qu'on a ignoré un
  `minimum_should_match` est le pire résultat possible de ce projet — pire que
  de ne pas supporter la clause du tout.
- **Ce qui est supporté est listé.** `docs/compat.md` tient l'inventaire de ce
  qui est implémenté, partiellement implémenté, et refusé — mis à jour dans la
  PR qui change le comportement, pas après.

## ✅ Check-list avant de passer la carte en `to be tested`

- [ ] `cargo build --release`, `cargo test`, `cargo clippy --all-targets -- -D warnings`
      et `cargo fmt --check` verts.
- [ ] Le comportement est **vérifié en vrai** : serveur lancé, requête exercée,
      résultat constaté — et via le **client officiel** dès que la
      fonctionnalité touche à la surface d'API.
- [ ] `docs/compat.md` reflète l'état réel du support.
- [ ] **Preuve jointe à la carte** : la sortie réellement constatée (réponse du
      client, sortie des tests de compat), pas « tests OK ».
- [ ] Ce fichier reflète toujours les commandes réelles du repo.

## 1. Implémenter

Réalise la demande (titre + propriété `Idea` + corps de la carte, rappelés dans
ton prompt). Commit au fur et à mesure sur la branche courante.

## 2. Vérifier

```bash
cargo run --release &                    # ou `cargo run` en dev
sleep 1

# La poignée de main que fait tout client officiel au démarrage :
curl -s localhost:9200/ | jq            # version, cluster_name, tagline
curl -s -i localhost:9200/ | grep -i x-elastic-product   # header obligatoire pour les clients 8.x

# Puis le vrai juge de paix :
python3 - <<'PY'
from elasticsearch import Elasticsearch
es = Elasticsearch("http://localhost:9200")
print(es.info())
PY
```

Un client officiel qui refuse de parler au serveur (souvent à cause du header
`X-elastic-product`, ou d'un numéro de version qu'il juge incompatible) est un
**échec de la carte**, même si tous les `curl` passent.

## 3. Faire avancer la carte, puis ouvrir la PR

Une fois la check-list verte :

```bash
# 1) statut -> "to be tested"  (Status est un select Notion, pas un status)
curl -s -X PATCH "https://api.notion.com/v1/pages/$NOTION_PAGE_ID" \
  -H "Authorization: Bearer $NOTION_TOKEN" \
  -H "Notion-Version: 2022-06-28" -H "Content-Type: application/json" \
  -d '{"properties":{"Status":{"select":{"name":"to be tested"}}}}'

# 2) push + PR
git add -A && git commit -m "..." && git push -u origin HEAD
gh pr create --title "..." --body "..."
```

Puis commente le lien de la PR sur la carte (`POST /v1/comments`) avec ce que tu
as réellement vérifié (les sorties constatées), et passe la carte en `in review` :

```bash
curl -s -X PATCH "https://api.notion.com/v1/pages/$NOTION_PAGE_ID" \
  -H "Authorization: Bearer $NOTION_TOKEN" \
  -H "Notion-Version: 2022-06-28" -H "Content-Type: application/json" \
  -d '{"properties":{"Status":{"select":{"name":"in review"}}}}'
```

Au merge de la PR, un webhook GitHub repasse la carte en `completed` et arrête
le worker — rien à faire de plus.

## Publier une version

Une release = un **tag sur `main`**. Mais le tag vient après une PR de bump, et
cette PR porte une étape qu'aucune version précédente n'avait écrite : **un bump
de version invalide par construction la mesure du conteneur.**

`docs/container.json` porte la version qui a été *mesurée* — le tag de l'image
et le `build_hash` que le binaire a annoncé sur `GET /` ; `Cargo.toml` porte
celle qu'on *déclare*. Le cliquet `chiffres_conteneur.py --verifie` (job CI « le
périmètre déclaré ») échoue dès que les deux divergent, et c'est exactement ce
qu'on lui demande : sans lui, un binaire qui grossit republierait la taille de
l'ancien sous son nouveau numéro. La release 0.8.0 s'est arrêtée là — le
garde-fou a fonctionné, rien n'était en panne. Ce qui manquait, c'est que
personne ne jouait la commande qu'il nomme.

Le garde-fou n'est donc pas à assouplir ; la commande qu'il nomme est à
**jouer**, dans la PR de bump :

```bash
# 1) le bump : Cargo.toml, Cargo.lock, et l'archive citée sous « Installer »
#    dans le README (`ferrite-v0.8.0-x86_64-…`). Ne PAS toucher aux blocs
#    <!-- chiffres-conteneur:… --> : ils sont générés, et un remplacement de
#    version à l'intérieur d'un de ces blocs fait échouer le cliquet.

# 2) la campagne, à la version qu'on vient de déclarer. Elle mesure les DEUX
#    images (ferrite et l'ES de référence) dans la même campagne, sur la même
#    machine : c'est la seule façon de tenir « même définition des deux côtés ».
#    Docker requis ; le build et la campagne prennent une dizaine de minutes.
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
docker build -t "ferrite:$version" .
./tests/compat/measure_container.sh --json docs/container.json

# 3) publier les chiffres mesurés : README.md, docs/bench.md, CLAUDE.md
python3 tests/compat/chiffres_conteneur.py --injecte

# 4) le cliquet, celui que la CI relance
python3 tests/compat/chiffres_conteneur.py --verifie
```

La campagne refuse de mesurer autre chose que ce qui est déclaré : image absente
(elle nomme le `docker build`), tag qui ne suit pas `Cargo.toml`, ou `build_hash`
d'un binaire qui n'est pas celui de la version. Un tag dit ce qu'on a *voulu*
construire ; c'est le serveur qui répond au `GET /` chronométré qui dit ce qu'on
a *mesuré*.

Une note sur le RSS et le démarrage, puisqu'ils dépendent de la machine (le
rapport la nomme, dans `mesure.hote`) : ce qui doit partager une machine, c'est
la mesure des **deux images d'une même campagne** — pas deux campagnes
successives. Une release mesurée sur un autre runner reste donc valide ; c'est
comparer son démarrage à celui de la release précédente qui ne l'est pas.

Puis, la PR de bump mergée :

```bash
git tag v0.8.0 && git push origin v0.8.0
```

`.github/workflows/release.yml` compile alors le binaire statique musl sur un
runner **x86-64** et un runner **arm64** (pas de cross-compilation), attache les
deux archives `.tar.gz` + `.sha256` à une release GitHub, et génère les notes
depuis les PR mergées depuis le tag précédent.

Le workflow **refuse** un tag posé sur un commit absent de `main` : une release
publiée depuis une branche de travail serait invérifiable après coup.

### Ce que l'automatisation doit jouer (`coderhammer/automations`)

La PR de release est ouverte par le job `cut-release`, qui vit dans
`coderhammer/automations` : ce dépôt-ci ne peut que le documenter et nommer ce
qu'il faut y changer. Ce que le job fait aujourd'hui s'arrête à l'étape 1, et il
remplace le numéro de version **partout** dans le README — y compris dans le
bloc généré (le diff de la PR #48 réécrivait la ligne
`| | Elasticsearch 8.15.0 | ferrite 0.8.0 | × |` à l'intérieur de
`<!-- chiffres-conteneur:tableau -->`). D'où, précisément :

- **jouer les étapes 2 à 4** entre le bump et le commit, et ajouter au commit
  `docs/container.json`, `README.md`, `docs/bench.md` et `CLAUDE.md` ;
- **ne plus remplacer la version dans les blocs `<!-- chiffres-conteneur:… -->`** :
  ils se régénèrent, ils ne se `sed` pas. Le seul remplacement légitime dans le
  README est le nom de l'archive citée sous « Installer » ;
- **donner Docker à son runner**, et le droit de tirer
  `docker.elastic.co/elasticsearch/elasticsearch:8.15.0` — la campagne mesure
  les deux images, une seule ne prouverait rien ;
- **si le runner ne peut pas jouer la campagne** : ouvrir la PR en `draft`, avec
  les quatre commandes dans son corps. Une PR de release présentée comme prête
  et rouge sur un garde-fou coûte une enquête à chaque version.

Ce qu'il ne faut **pas** faire, c'est apprendre au cliquet à laisser passer une
mesure « seulement un peu périmée ». La 0.8.0 le montre en deux nombres :
l'image a pris 2 028 octets depuis la 0.7.0 (4 149 171 → 4 151 199), et ces deux
mille octets déplacent le chiffre publié — **4,1 Mo devient 4,2 Mo**, sur le
premier argument du projet.
