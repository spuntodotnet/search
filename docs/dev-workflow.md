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
| `python3 tests/compat/diff_analyzers.py` | Compare les **analyzers** à ceux d'un vrai ES, token par token |
| `python3 tests/compat/diff_datemath.py [ferrite] [es]` | Compare la résolution des **bornes de date** — `now`, `now-1d/d`, `2026-03-15\|\|+1M`, et l'arrondi d'une borne selon son côté — documents rendus et messages d'erreur compris |
| `python3 tests/compat/diff_motifs.py` | Compare les **motifs** — `regexp`, `wildcard`, `prefix`, `match_phrase_prefix` — sur un corpus fait pour les pièges de la syntaxe de Lucene (casse, accents, caractères spéciaux) |
| `python3 tests/compat/sonde_msm.py [ferrite] [es]` | Compare les **notations de `minimum_should_match`** — entier, pourcentage, formes négatives, conditions `3<90%` — sur un `bool` et sous un `nested`, en posant à chaque serveur des requêtes dont le compte de résultats dit directement quel minimum a été appliqué |
| `python3 tests/compat/sonde_alias.py [ferrite] [es]` | Compare les **expressions de noms d'alias** de `GET /_alias/{nom}` — listes, jokers, exclusions, `_all` — statut, corps et message du 404 compris. C'est elle qui a montré que le tiret n'exclut qu'à partir du deuxième terme, et que la présence d'un joker change la règle du 404 |
| `python3 tests/compat/sonde_vide.py [ferrite] [es]` | Compare ce que rendent les deux serveurs quand ils n'ont **aucun index** — l'état que le harnais n'exerçait pas, et la raison pour laquelle le seul échec silencieux du projet y a vécu si longtemps. Sépare ce que les deux doivent rendre à l'octet près (requêtes valides, erreurs de lecture du corps) de ce que ferrite refuse alors qu'ES sait le faire. Refuse de tourner si un serveur n'est pas vide |
| `python3 tests/compat/diff_multi_index.py [ferrite] [es]` | Compare la **résolution des noms d'index** — listes, motifs, `_all`, exclusions, alias — et la **fusion multi-index** des résultats et des agrégations. `--calibrer [es_a] [es_b]` fait tourner la même batterie contre deux Elasticsearch, parce qu'une batterie qui modifie l'état du serveur ne peut pas s'étalonner contre un seul |
| `python3 tests/compat/probe_es7.py [URL]` | Exerce le serveur avec le client officiel **7.x** (code écrit pour un ES 7) : ce qu'un projet resté en 7.10.2 peut brancher tel quel — voir [`compat-es7.md`](compat-es7.md) |
| `python3 tests/compat/diff_es7.py [ferrite] [es7]` | Rejoue les **index, documents et requêtes** d'une instance **7.x** sur ferrite : ce qui s'héberge, ce qui se transfère, ce qui rend les mêmes résultats. Ne lit que l'instance (`--sans-ecriture` pour n'y rien écrire du tout ; `--inventaire URL` pour se contenter de lister les types de champ qu'elle utilise) |
| `python3 tests/compat/bench_vs_es.py [ferrite] [es]` | **Le banc** : mêmes documents et mêmes requêtes des deux côtés, puis indexation, latence, débit — et le compte des requêtes qui rendent le même résultat. Sans client Elasticsearch (HTTP brut), donc utilisable contre un ES 7.x comme 8.x |
| `python3 tests/compat/fuzz_vs_es.py [ferrite] [es] [--cas N]` | **Le fuzzer différentiel** : un mapping, des documents et des requêtes tirés au sort **dans le périmètre déclaré** (`compat.yaml` dit ce qui est jouable), envoyés aux deux serveurs, réponses comparées. `--calibrer [es_a] [es_b]` fait tourner la même batterie contre deux Elasticsearch — tant qu'elle n'y est pas à zéro, ce qu'il dit de ferrite ne vaut rien. `--rejouer <graine>` rejoue un cas en détail, `--couverture` imprime ce qu'il **ne** fuzze pas. Voir [`fuzz.md`](fuzz.md) |
| `python3 tests/compat/sonde_fuzz.py [ferrite] [es]` | Les écarts que le fuzzing a trouvés, **figés** : chacun réduit au plus petit mapping qui le montre, avec la phrase de ce qui était faux. Une graine ne se rejoue qu'à générateur constant ; un cas écrit, si |
| `python3 tests/compat/genere_compat.py [--verifie]` | Regénère [`docs/compat.md`](compat.md) et [`docs/compat.json`](compat.json) depuis [`compat.yaml`](../compat.yaml), la **source** du périmètre déclaré (le texte long reste écrit à la main dans [`compat.gabarit.md`](compat.gabarit.md)). `--verifie` est ce que lance la CI : elle échoue si le fichier commité diffère de sa source |
| `python3 tests/compat/recolte_usage.py` | Constitue le **corpus de vraies requêtes** ([`tests/compat/usage/corpus.jsonl`](../tests/compat/usage/corpus.jsonl)) depuis quatre sources publiques et citables : la doc de référence d'ES 8.15, les tracks Rally d'Elastic, les tests et exemples des clients officiels, et la recherche de code de GitHub. Chaque requête porte l'URL exacte d'où elle vient ; les dépôts sont clonés à la demande dans `.corpus-usage/` (ignoré par git) |
| `python3 tests/compat/ponderation.py [--json docs/usage.json] [--rejoue ferrite es] [--poids] [--verifie]` | **Ce que ce corpus réclame, et ce que ferrite en sert entièrement.** Croise chaque requête avec `compat.yaml` (une seule clause refusée fait tomber la requête), et `--rejoue` pose la même requête à ferrite et à un vrai ES 8.15 pour étalonner ce croisement. `--poids` écrit les `poids` de `compat.yaml`, `--verifie` est le cliquet de la CI. L'étude : [`usage.md`](usage.md) |
| `python3 tests/compat/perimetre.py [api] [message]` | Rattache un cas échoué de la suite de conformance à une capacité de `compat.yaml` — et donc dit si c'est une **régression** ou un **coût de périmètre**. Sans argument, imprime l'index tel qu'il est lu |
| `python3 tests/compat/conformance_es.py [URL] [--json <fichier>] [--diff <ancien.json>]` | Rejoue la **suite de conformance REST d'Elasticsearch lui-même** (7.10.2, Apache 2.0, téléchargée à la demande dans `.es-rest-spec/`), ses **107 domaines**, sans liste blanche — le tri se fait dans le rapport, pas par omission. Les cas ne viennent pas de nous : c'est ce qui attrape ce qu'on ne sait pas qu'on ignore. `--json` écrit le rapport machine ([`docs/conformance.json`](conformance.json), commité, source de tous les chiffres publiés) ; `--diff` dit ce qui a bougé depuis un rapport et fait du code de sortie un **cliquet** (celui du job CI `conformance`) |
| `./tests/compat/measure_container.sh [tag]` | Ne construit rien, mesure une image déjà buildée : taille, RSS au repos, temps de démarrage |
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
| `tests/compat/diff_aggs.py` | les **agrégations** — 53 requêtes, comparaison du JSON champ par champ, clés comprises (dont 11 sur l'agrégation `filter`, que ferrite exécute lui-même, et 8 sur ce qu'un bucket **vide** doit porter) |
| `tests/compat/diff_analyzers.py` | les **analyzers** — chaque analyzer intégré confronté à son homonyme d'ES sur 28 textes, token par token |
| `tests/compat/diff_datemath.py` | les **bornes de date** — 276 bornes posées aux deux serveurs sur un corpus d'instants placés sur les bords (minuit, dernière milliseconde d'un jour, d'un mois, d'une année) : une milliseconde d'arrondi de travers change la réponse. Le même fichier lancé contre le ferrite d'avant rend 45/276 — c'est ce qui prouve qu'il mesure quelque chose |
| `tests/compat/diff_motifs.py` | les **motifs** — 101 motifs posés aux deux serveurs sur un corpus construit pour eux : la syntaxe de `regexp` est celle de Lucene, pas celle du moteur qui l'exécute, et les deux divergent là où personne ne regarde (`\d`, `^`, `@`, `case_insensitive`) |
| `tests/compat/sonde_msm.py` | les notations de **`minimum_should_match`** — 53 requêtes dont le compte de résultats dit quel minimum a été appliqué, sur un `bool` et sous un `nested`. C'est elle qui a montré que l'arrondi d'ES est une troncature vers zéro, qu'un minimum supérieur au nombre de clauses n'est pas plafonné, et que le séparateur de `2<-25% 9<-3` est l'espace |
| `tests/compat/diff_multi_index.py` | les **expressions d'index** et le multi-index — `es.search(index=["a","b"])`, `logs-*`, `_all`, exclusions, alias, `is_write_index`, purge en `DELETE /logs-2026.07.*` : total, ordre des `(_index, _id)`, `_shards`, agrégations fusionnées, statut et type d'erreur |
| `tests/compat/probe_es7.py` | ce qu'un **client 7.x** obtient — le même fichier se lance contre ferrite, contre un `elasticsearch:7.10.2` et contre un `elasticsearch:8.15.0`, ce qui sépare « ferrite est incomplet » de « la 8 a supprimé ça » |
| `tests/compat/bench_vs_es.py` | le **prix** de ces résultats — indexation, latence médiane et p95, débit à 8 requêtes en vol, mesurés sur les deux serveurs avec la même batterie |
| `tests/compat/sonde_alias.py` | les **expressions de noms d'alias** — 21 expressions posées aux deux serveurs, choisies pour séparer les lectures possibles de la règle. C'est elle qui a montré que `test_alias_1,-test` rend 404 là où `test_blias_2,test_alias*,-test_alias_1` rend 200 : la même exclusion d'un alias qui existe, et c'est le joker qui les sépare |
| `tests/compat/sonde_vide.py` | le **serveur vide** — 28 questions posées aux deux, dont le corps entier se compare sur un 200 : `_shards` à zéro, `max_score` à `0.0` (et non `null`), pas de section `aggregations`. C'est elle qui a montré qu'ES refuse sans index ce qu'il refuse avec, et seulement ça |
| `tests/compat/conformance_es.py` | les tests d'**Elastic**, pas les nôtres — la suite REST officielle, ses 107 domaines. Se valide en le lançant contre un vrai ES 7.10.2, où il doit être quasi tout vert ([`docs/conformance-es7102.json`](conformance-es7102.json)) — le conteneur doit alors porter les réglages du cluster de test d'Elastic (`node.attr.testattr`, `path.repo`), voir [`conformance.md`](conformance.md). Sa mesure sur ferrite est [`docs/conformance.json`](conformance.json) : totaux, taux, exclusions comptées, détail par cas |
| `tests/compat/diff_es7.py` | ce qu'une **instance 7.x** peut céder à ferrite — ses index (rejoués), ses documents (`scan` + `bulk`), ses requêtes (même corpus, même ordre attendu) |
| `tests/compat/ponderation.py --rejoue` | ce que **5 311 vraies requêtes** deviennent : la même posée aux deux serveurs sur un index vide, mais dont le mapping est déduit de la requête elle-même (sans quoi toute agrégation serait refusée faute de champ). Ne compare qu'« accepté / refusé » — ce qu'ES refuse aussi sort du dénominateur. C'est lui qui a montré qu'un champ non mappé avalait le refus de `range`, `term`, `terms` et `regexp` |
| `tests/compat/fuzz_vs_es.py` | ce à quoi **personne n'a pensé** — mapping, documents et requêtes tirés au sort dans le périmètre déclaré, comparés champ par champ. Il s'étalonne contre deux Elasticsearch avant de servir, et chaque divergence qu'il laisse passer porte un prédicat écrit, pas un code d'état toléré en bloc |
| `tests/compat/sonde_fuzz.py` | les écarts déjà trouvés par le fuzzing, **figés** — ils ne dépendent plus d'une graine |

Le second est celui qui compte pour un moteur de recherche : l'ordre des
résultats est précisément ce qu'un test écrit à la main ne sait pas vérifier,
puisqu'on l'écrirait avec la même idée fausse que le code. Il signale un écart
d'ordre sauf s'il ne porte que sur des documents qu'Elasticsearch lui-même
classe ex æquo.

Le corpus vit dans `tests/compat/corpus.py` et est généré avec une graine fixe :
un écart constaté est toujours reproductible.

Ce sont des outils de développement, pas des tests de CI : ils exigent Docker.

Une exception : `conformance_es.py` n'a besoin que de ferrite et de la suite
d'Elastic (téléchargée, mise en cache). Le job CI `conformance` le lance donc
contre un ferrite fraîchement compilé et compare au rapport commité
`docs/conformance.json` — c'est un **cliquet** : il échoue si le nombre d'échecs
augmente, ou si un cas passe de réussi à échec. Une PR qui fait bouger la mesure
régénère le rapport dans la même PR (`--json docs/conformance.json`).

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

Une release = un **tag sur `main`**, rien d'autre :

```bash
git tag v0.1.0 && git push origin v0.1.0
```

`.github/workflows/release.yml` compile alors le binaire statique musl sur un
runner **x86-64** et un runner **arm64** (pas de cross-compilation), attache les
deux archives `.tar.gz` + `.sha256` à une release GitHub, et génère les notes
depuis les PR mergées depuis le tag précédent.

Le workflow **refuse** un tag posé sur un commit absent de `main` : une release
publiée depuis une branche de travail serait invérifiable après coup.
