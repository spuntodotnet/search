# La suite de conformance d'Elasticsearch, passée à ferrite

Tout le reste du harnais compare ferrite à Elasticsearch sur des cas **qu'on a
écrits**. C'est utile, et c'est limité de la même façon que le code : on teste
ce à quoi on a pensé. Cette page-ci rapporte l'inverse — les cas viennent
d'Elastic.

```bash
python3 tests/compat/conformance_es.py http://localhost:9200                    # à l'écran
python3 tests/compat/conformance_es.py http://localhost:9200 --json docs/conformance.json
python3 tests/compat/conformance_es.py http://localhost:9200 --diff docs/conformance.json
```

## La source des chiffres est un fichier, pas cette page

Tant qu'un compte n'existe que sous forme de prose, personne d'autre ne peut le
lire : ni la CI, ni le lecteur qui veut vérifier. Les chiffres de cette mesure
vivent donc dans **[`conformance.json`](conformance.json)**, commité, régénéré
par la commande ci-dessus — et cette page n'en recopie aucun.

| Fichier | Ce qu'il contient |
|---|---|
| [`conformance.json`](conformance.json) | la mesure contre **ferrite** |
| [`conformance-es7102.json`](conformance-es7102.json) | la même mesure contre un **vrai Elasticsearch 7.10.2** — l'étalonnage de l'instrument |

Chaque rapport porte de quoi savoir ce qu'il vaut : la version de la suite REST
utilisée, la date de la mesure, le SHA du dépôt mesuré (et s'il était modifié),
la version annoncée par la cible, puis les totaux, le détail **par suite** et
**par cas** (fichier, nom du cas, catégorie, et pour un échec la raison courte).

Ce SHA est celui du commit **mesuré**, donc forcément antérieur au commit qui
range le rapport — un rapport ne peut pas citer le commit qui le contient. Ce
qui compte est `ferrite_arbre_modifie` : à `false`, la mesure vient d'un arbre
de travail propre, donc d'un code qu'on peut retrouver.

Les totaux et les deux taux, sans rien installer :

```bash
python3 - <<'PY'
import json
r = json.load(open("docs/conformance.json"))
print(r["mesure"]["date"], r["mesure"]["cible"]["version_annoncee"])
print(r["totaux"])
for nom, t in r["taux"].items():
    print(f'{nom}: {t["numerateur"]}/{t["denominateur"]}')
PY
```

## Quatre catégories, deux taux

Les catégories ne mesurent pas la même chose, et c'est la distinction qui
compte :

| Catégorie | Ce qu'elle veut dire |
|---|---|
| `reussi` | ferrite répond comme Elasticsearch |
| `refus` | ferrite répond « je ne sais pas faire » (`not_implemented_in_ferrite_exception`) : c'est le contrat, pas un bug |
| `saute` | le cas ne mesure pas la cible — borne de version de la suite, ou verbe que ce runner n'implémente pas |
| `echec` | ferrite répond **autre chose** qu'Elasticsearch : ce sont les seuls vrais écarts |

D'où deux taux, qui ne répondent pas à la même question :

- **fidélité dans le périmètre déclaré** = `reussis / (reussis + echecs)`.
  Parmi les cas qui n'exercent que des capacités déclarées supportées — ceux que
  ferrite n'a ni refusés ni fait sauter — combien passent. C'est le taux qui dit
  si **ce qu'on annonce est juste**.
- **couverture brute** = `reussis / total`. Quelle part de la suite d'Elastic
  passe, périmètre non déclaré compris. C'est le taux qui dit **quelle part
  d'Elasticsearch on couvre**.

Confondre les deux, c'est se flatter (ne citer que le premier) ou se punir (ne
citer que le second). Le JSON porte les deux, chacun avec son numérateur et son
dénominateur, pour qu'ils se recalculent.

Un biais assumé : une partie des `echec` sont en réalité des refus, mais dont le
type d'erreur imite Elasticsearch (`parsing_exception` sur `unknown query
[intervals]`, par exemple) au lieu de porter le marqueur
`not_implemented_in_ferrite_exception`. Ils comptent donc contre la fidélité.
Compter « juste » demanderait de mentir sur le type d'erreur ; on préfère la
fidélité du message et une colonne moins flatteuse.

## D'où viennent ces tests, et pourquoi on a le droit

Elasticsearch publie ses tests REST au format YAML : une suite d'appels et
d'assertions, conçue pour être rejouée par n'importe quel client contre
n'importe quel serveur. **La 7.10.2 est la dernière version publiée sous Apache
2.0** — la 7.11 bascule en SSPL + Elastic License. C'est donc celle-là qu'on
utilise, et sa licence est compatible avec celle de ferrite.

Heureuse coïncidence : c'est aussi la version de l'instance qu'on cherche à
servir (voir [`compat-es7.md`](compat-es7.md)).

Les fichiers ne sont **pas recopiés** dans ce dépôt : ils sont téléchargés à la
demande dans `.es-rest-spec/`, ignoré par git.

## Le runner se valide avant de mesurer

Un runner qui échoue partout ne prouve rien sur ferrite. Il est donc d'abord
lancé contre un **vrai Elasticsearch 7.10.2**, où il doit être quasi tout vert :

```bash
docker run -d --name es-ref-7102 -p 9202:9200 -e discovery.type=single-node \
  -e ES_JAVA_OPTS="-Xms512m -Xmx512m" \
  docker.elastic.co/elasticsearch/elasticsearch:7.10.2
python3 tests/compat/conformance_es.py http://localhost:9202 \
  --json docs/conformance-es7102.json
```

Le résultat est dans [`conformance-es7102.json`](conformance-es7102.json) — et
les quelques échecs qui y restent sont ES lui-même (`distance_feature` sur
`date` et `date_nanos`, une agrégation `range` sur un champ non mappé : « all
shards failed » dans ce conteneur). Ils sont comptés, pas cachés.

Écrire ce runner a d'ailleurs coûté quatre corrections successives, toutes
révélées par ce passage contre ES et **aucune** attribuable à ferrite : un
nettoyage qui échouait sur un index en lecture seule et faisait cascader 400
faux échecs, le NDJSON pré-sérialisé, le paramètre `ignore` du runner officiel
pris pour un paramètre d'API, et les réponses `_cat` en texte plutôt qu'en JSON.
C'est exactement pour ça qu'on ne mesure pas avant d'avoir étalonné l'instrument.

## Le cliquet, en CI

Le job `conformance` de la CI compile ferrite, le lance, rejoue la suite et
compare la mesure fraîche au rapport commité :

```bash
python3 tests/compat/conformance_es.py http://127.0.0.1:9200 \
  --json conformance-mesure.json --diff docs/conformance.json
```

Avec `--diff`, le code de sortie devient un **cliquet** : il vaut 1 si le nombre
d'échecs augmente, ou si un cas passe de `reussi` à `echec` — même à total
constant. Ce n'est pas une cible : le job ne dit rien du nombre d'échecs, il dit
seulement qu'il n'a pas monté. Sans `--diff`, le code de sortie garde son sens
d'origine (1 dès qu'il reste un échec).

La sortie du diff nomme ce qui a bougé dans les deux sens — les cas passés
d'échec à réussi, et l'inverse — plus les autres mouvements (un cas qui passe de
`refus` à `reussi`, par exemple) et les cas apparus ou disparus.

Une PR qui fait bouger la mesure régénère `docs/conformance.json` dans la même
PR : le cliquet, lui, laisse passer une amélioration sans exiger la mise à jour,
sinon il deviendrait une cible.

## Ce que ça a trouvé

Le premier passage a sorti **deux manques francs**, qu'aucun test écrit ici
n'avait vus, tous deux corrigés depuis :

| Ce que c'était | Corrigé |
|---|---|
| `no such index [test_1]` — les tests indexent sans créer l'index | ✅ l'écriture crée l'index à la volée (`index`, `create`, `update`, `_bulk`), comme ES. La **lecture** et la **suppression** rendent toujours 404, comme ES aussi |
| `Invalid index name [_refresh]`, `[_all]`, `[_mapping]` | ✅ `POST /_refresh`, `GET /_mapping`, et `_all` / `*` sur les routes administratives |
| `{"type": "object"}` sans `properties` refusé | ✅ accepté, l'objet ne déclare rien et ses champs viendront des documents |

Le support des **expressions d'index et des alias** a ensuite fait bouger le
compte une seconde fois, et d'une façon que le total cache : une centaine de cas
ont quitté la colonne « refusé » sans devenir « réussi ». Ils n'y étaient que
parce que ferrite refusait la **route** avant d'arriver au vrai sujet — un
`POST /_search` sans index était refusé d'emblée, donc les cas d'agrégation qui
l'utilisent ne mesuraient rien. Maintenant qu'ils vont au bout, ils montrent ce
qui manquait vraiment derrière.

Un de ces retours en arrière a valu correctif : `rest_total_hits_as_int` était
refusé comme un **paramètre inconnu**, alors qu'ES le connaît. Il est désormais
refusé pour ce qu'il est (`not_implemented_in_ferrite_exception`), ce qui rend
son refus lisible côté client.

`scroll`, l'agrégation `filter` et les champs non mappés ont fait bouger le
compte une troisième fois, et le mouvement a été petit — c'est instructif : la
suite REST de la 7.10 n'exerce le `scroll` que dans un seul cas (`field
collapsing and scroll`, qui bute d'abord sur `collapse`), et l'écrasante
majorité de ses cas d'agrégation sont déjà refusés pour d'autres raisons. Une
fonctionnalité peut être décisive pour un vrai projet — celle-ci débloque tout
export d'index — sans faire bouger un compteur global. C'est pour ça que ce
fichier n'est pas la seule mesure du dépôt.

## Ce qui reste, et comment le compter soi-même

Les familles d'échecs qui restent. Aucun compte n'est recopié ici — il serait
faux à la PR suivante ; celui du jour se sort du rapport :

```bash
python3 - <<'PY'
import collections, json, re
cas = json.load(open("docs/conformance.json"))["cas"]
familles = collections.Counter(
    re.sub(r"\[[^]]*\]", "[…]", c["raison"])[:60]
    for c in cas if c["categorie"] == "echec")
for raison, n in familles.most_common(15):
    print(f"{n:>4}  {raison}")
PY
```

| Ce que c'est | Verdict |
|---|---|
| `unknown query [intervals]`, `unrecognized parameter: [version]`… | des refus **réels**, mais dont le type d'erreur imite celui d'Elasticsearch au lieu de porter le marqueur `not_implemented_in_ferrite_exception` |
| `include_type_name`, `_type` attendu dans la réponse | la suite est celle de la 7.10 ; ces cas testent ce que la **8.x a supprimé**. Un vrai ES 8 y échoue aussi |
| `indices.get` bloqués sur `_close` dans leur **mise en place** | l'échec n'est pas sur ce qu'ils mesurent |
| `_close` / `_open` | hors périmètre déclaré |
| `indices.delete` sur un motif ou sur un alias | ferrite suit la 8.x : `action.destructive_requires_name` vaut `true` et `DELETE /{alias}` est refusé. La suite est celle de la **7.10**, où le réglage valait `false` — un vrai ES 8 échoue au même endroit |
| `collapse`, `docvalue_fields`, `stored_fields`, agrégations non supportées… | hors périmètre déclaré, voir [`compat.md`](compat.md) |

## Ce que le runner ne fait pas

- Les domaines hors périmètre déclaré ne sont pas lancés (snapshots, ILM,
  cluster distribué, scripts…) : les mesurer ne dirait rien de plus que
  `compat.md`. La liste exacte des domaines joués est dans le rapport
  (`mesure.suites`).
- Les fichiers `*_with_types.yml` sont ignorés : ils testent l'API typée
  (`/{index}/{type}/{id}`), supprimée en 8.x.
- Les `skip: {version}` sont évalués **comme pour un serveur 7.10.2**, puisque
  c'est la version de la suite.
- `--suites` ne joue qu'une partie des domaines : le rapport le dit
  (`mesure.partiel`), et `--diff` refuse alors de trancher — une mesure
  partielle ne se compare pas à la suite entière.

Ces exclusions sont volontaires et comptées : le rapport porte le nombre exact
de cas retenus, sur les ~1 500 du dépôt d'Elastic.
