# dev-workflow — ferrite

> **Fichier lu par l'agent qui tourne sur le worker Spunto** dans le pipeline
> idée→prod. Le service `automations` ne fait que **créer** ce worker (carte
> Notion passée en `running`) et **l'arrêter** (au merge de la PR → `completed`).
> Tout le reste — implémenter, vérifier, faire avancer la carte, ouvrir la PR —
> c'est **toi**, l'agent, qui le pilotes en suivant ce fichier.

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
| `cargo test` | Tests unitaires + intégration |
| `cargo clippy --all-targets -- -D warnings` | Lint, **zéro warning toléré** |
| `cargo fmt --check` | Vérifie le formatage (`cargo fmt` pour corriger) |
| `./tests/compat/run.sh` | **Le harnais de compat** : compile, lance ferrite sur un port jetable, et l'exerce avec le client Elasticsearch officiel (critère d'acceptation + suite complète) |
| `./tests/compat/measure_container.sh [tag]` | Construit rien, mesure une image déjà buildée : taille, RSS au repos, temps de démarrage |
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
python3 tests/compat/diff_against_es.py         # diff champ par champ
```

`diff_against_es.py` envoie la même suite d'appels aux deux serveurs via le
client officiel et compare chaque réponse, champ par champ, après
neutralisation des valeurs qui ne peuvent pas coïncider (durées, uuid, scores).
C'est un outil de développement, pas un test de CI : il exige Docker.

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
