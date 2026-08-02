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
| `python3 tests/compat/diff_motifs.py` | Compare les **motifs** — `regexp`, `wildcard`, `prefix`, `match_phrase_prefix` — sur un corpus fait pour les pièges de la syntaxe de Lucene (casse, accents, caractères spéciaux) |
| `python3 tests/compat/diff_multi_index.py [ferrite] [es]` | Compare la **résolution des noms d'index** — listes, motifs, `_all`, exclusions, alias — et la **fusion multi-index** des résultats et des agrégations. `--calibrer [es_a] [es_b]` fait tourner la même batterie contre deux Elasticsearch, parce qu'une batterie qui modifie l'état du serveur ne peut pas s'étalonner contre un seul |
| `python3 tests/compat/probe_es7.py [URL]` | Exerce le serveur avec le client officiel **7.x** (code écrit pour un ES 7) : ce qu'un projet resté en 7.10.2 peut brancher tel quel — voir [`compat-es7.md`](compat-es7.md) |
| `python3 tests/compat/diff_es7.py [ferrite] [es7]` | Rejoue les **index, documents et requêtes** d'une instance **7.x** sur ferrite : ce qui s'héberge, ce qui se transfère, ce qui rend les mêmes résultats. Ne lit que l'instance (`--sans-ecriture` pour n'y rien écrire du tout ; `--inventaire URL` pour se contenter de lister les types de champ qu'elle utilise) |
| `python3 tests/compat/bench_vs_es.py [ferrite] [es]` | **Le banc** : mêmes documents et mêmes requêtes des deux côtés, puis indexation, latence, débit — et le compte des requêtes qui rendent le même résultat. Sans client Elasticsearch (HTTP brut), donc utilisable contre un ES 7.x comme 8.x |
| `python3 tests/compat/conformance_es.py [URL]` | Rejoue la **suite de conformance REST d'Elasticsearch lui-même** (7.10.2, Apache 2.0, téléchargée à la demande dans `.es-rest-spec/`). Les cas ne viennent pas de nous : c'est ce qui attrape ce qu'on ne sait pas qu'on ignore |
| `./tests/compat/measure_container.sh [tag]` | Ne construit rien, mesure une image déjà buildée : taille, RSS au repos, temps de démarrage |
| `docker build -t ferrite .` | Image minimale (`scratch` + binaire statique musl) |

Le harnais de compat installe le client officiel dans un venv (`.venv-compat/`)
s'il n'est pas déjà disponible. Il accepte `FERRITE_PORT` (port d'écoute) et
`FERRITE_URL` (viser un serveur déjà lancé, sans rien compiler).

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
| `tests/compat/diff_against_es.py` | la **forme** des réponses — champ par champ, sur une quarantaine d'appels, après neutralisation des valeurs qui ne peuvent pas coïncider (durées, uuid, scores) |
| `tests/compat/diff_relevance.py` | la **pertinence** — même corpus de 600 documents des deux côtés, ~115 requêtes générées, et pour chacune : même total, mêmes documents, **même ordre** |
| `tests/compat/diff_aggs.py` | les **agrégations** — 34 requêtes, comparaison du JSON champ par champ, clés comprises |
| `tests/compat/diff_analyzers.py` | les **analyzers** — chaque analyzer intégré confronté à son homonyme d'ES sur 28 textes, token par token |
| `tests/compat/diff_motifs.py` | les **motifs** — 101 motifs posés aux deux serveurs sur un corpus construit pour eux : la syntaxe de `regexp` est celle de Lucene, pas celle du moteur qui l'exécute, et les deux divergent là où personne ne regarde (`\d`, `^`, `@`, `case_insensitive`) |
| `tests/compat/diff_multi_index.py` | les **expressions d'index** et le multi-index — `es.search(index=["a","b"])`, `logs-*`, `_all`, exclusions, alias, `is_write_index`, purge en `DELETE /logs-2026.07.*` : total, ordre des `(_index, _id)`, `_shards`, agrégations fusionnées, statut et type d'erreur |
| `tests/compat/probe_es7.py` | ce qu'un **client 7.x** obtient — le même fichier se lance contre ferrite, contre un `elasticsearch:7.10.2` et contre un `elasticsearch:8.15.0`, ce qui sépare « ferrite est incomplet » de « la 8 a supprimé ça » |
| `tests/compat/bench_vs_es.py` | le **prix** de ces résultats — indexation, latence médiane et p95, débit à 8 requêtes en vol, mesurés sur les deux serveurs avec la même batterie |
| `tests/compat/conformance_es.py` | les tests d'**Elastic**, pas les nôtres — 643 cas de la suite REST officielle. Se valide en le lançant contre un vrai ES 7.10.2, où il doit être quasi tout vert (537/643, 3 échecs côté ES) |
| `tests/compat/diff_es7.py` | ce qu'une **instance 7.x** peut céder à ferrite — ses index (rejoués), ses documents (`scan` + `bulk`), ses requêtes (même corpus, même ordre attendu) |

Le second est celui qui compte pour un moteur de recherche : l'ordre des
résultats est précisément ce qu'un test écrit à la main ne sait pas vérifier,
puisqu'on l'écrirait avec la même idée fausse que le code. Il signale un écart
d'ordre sauf s'il ne porte que sur des documents qu'Elasticsearch lui-même
classe ex æquo.

Le corpus vit dans `tests/compat/corpus.py` et est généré avec une graine fixe :
un écart constaté est toujours reproductible.

Ce sont des outils de développement, pas des tests de CI : ils exigent Docker.

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
