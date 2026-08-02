# La suite de conformance d'Elasticsearch, passée à ferrite

Tout le reste du harnais compare ferrite à Elasticsearch sur des cas **qu'on a
écrits**. C'est utile, et c'est limité de la même façon que le code : on teste
ce à quoi on a pensé. Cette page-ci rapporte l'inverse — les cas viennent
d'Elastic.

```bash
python3 tests/compat/conformance_es.py http://localhost:9200
```

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

| | ferrite | ES 7.10.2 |
|---|---|---|
| réussis | **65** | **537** |
| refusés explicitement (hors périmètre) | 324 | 0 |
| sautés (borne de version, fonctionnalité du runner) | 98 | 103 |
| **échecs** | **156** | **3** |
| | | sur 643 cas |

Les 3 échecs côté ES sont ES lui-même (`distance_feature` sur `date` et
`date_nanos`, une agrégation `range` sur un champ non mappé : « all shards
failed » dans ce conteneur). Ils sont comptés, pas cachés.

Écrire ce runner a d'ailleurs coûté quatre corrections successives, toutes
révélées par ce passage contre ES et **aucune** attribuable à ferrite : un
nettoyage qui échouait sur un index en lecture seule et faisait cascader 400
faux échecs, le NDJSON pré-sérialisé, le paramètre `ignore` du runner officiel
pris pour un paramètre d'API, et les réponses `_cat` en texte plutôt qu'en JSON.
C'est exactement pour ça qu'on ne mesure pas avant d'avoir étalonné l'instrument.

## Ce que ça a trouvé

Les colonnes ne mesurent pas la même chose. **« refusé »** veut dire que ferrite
répond « je ne sais pas faire », ce qui est le contrat ; **« échec »** veut dire
qu'il répond autre chose qu'Elasticsearch. Seuls les seconds sont des écarts.

Le premier passage donnait 24 réussis et 218 échecs. Il en est ressorti **deux
manques francs**, tous deux corrigés depuis — c'est ce qui a fait passer le
compte à 44 réussis et 169 échecs :

| Combien | Ce que c'était | Corrigé |
|---|---|---|
| **58** | `no such index [test_1]` — les tests indexent sans créer l'index | ✅ l'écriture crée l'index à la volée (`index`, `create`, `update`, `_bulk`), comme ES. La **lecture** et la **suppression** rendent toujours 404, comme ES aussi |
| **34** | `Invalid index name [_refresh]`, `[_all]`, `[_mapping]` | ✅ `POST /_refresh`, `GET /_mapping`, et `_all` / `*` sur les routes administratives |
| **3** | `{"type": "object"}` sans `properties` refusé | ✅ accepté, l'objet ne déclare rien et ses champs viendront des documents |

Le support des **expressions d'index et des alias** a ensuite fait bouger le
compte une seconde fois, à 65 réussis et 156 échecs. Le mouvement est plus
grand que ces chiffres ne le laissent voir : 159 cas ont quitté la colonne
« refusé ». Ils n'y étaient que parce que ferrite refusait la **route** avant
d'arriver au vrai sujet — un `POST /_search` sans index était refusé d'emblée,
donc la centaine de cas d'agrégation qui l'utilisent ne mesurait rien.
Maintenant qu'ils vont au bout, 29 passent et le reste montre ce qui manquait
vraiment derrière.

Un de ces retours en arrière a valu correctif : `rest_total_hits_as_int` était
refusé comme un **paramètre inconnu**, alors qu'ES le connaît. Il est désormais
refusé pour ce qu'il est (`not_implemented_in_ferrite_exception`), ce qui rend
son refus lisible côté client — et rend au passage 100 cas d'agrégation à la
colonne « refusé », où ils sont chez eux.

Ce qui reste dans les 156 :

| Combien | Ce que c'est | Verdict |
|---|---|---|
| **~40** | `unknown query [intervals]`, `unrecognized parameter: [version]`… | des refus **réels**, mais dont le type d'erreur imite celui d'Elasticsearch (`parsing_exception`) au lieu de porter le marqueur `not_implemented_in_ferrite_exception`. Compter juste demanderait de mentir sur le type ; on préfère la fidélité et une colonne moins flatteuse |
| **~18** | `include_type_name`, `_type` attendu dans la réponse | la suite est celle de la 7.10 ; ces cas testent ce que la **8.x a supprimé**. Un vrai ES 8 y échoue aussi |
| **12** | `indices.get` | bloqués sur `_close` dans leur **mise en place**, pas sur ce qu'ils mesurent |
| **7** | `_close` / `_open` | hors périmètre déclaré |
| **6** | `indices.delete` sur un motif ou sur un alias | ferrite suit la 8.x : `action.destructive_requires_name` vaut `true` et `DELETE /{alias}` est refusé. La suite est celle de la **7.10**, où le réglage valait `false` — un vrai ES 8 échoue au même endroit |
| reste | `collapse`, `docvalue_fields`, `stored_fields`, agrégations non supportées, `scroll`… | hors périmètre déclaré, voir [`compat.md`](compat.md) |

## Ce que le runner ne fait pas

- Les domaines hors périmètre déclaré ne sont pas lancés (snapshots, ILM,
  cluster distribué, scripts…) : les mesurer ne dirait rien de plus que
  `compat.md`.
- Les fichiers `*_with_types.yml` sont ignorés : ils testent l'API typée
  (`/{index}/{type}/{id}`), supprimée en 8.x.
- Les `skip: {version}` sont évalués **comme pour un serveur 7.10.2**, puisque
  c'est la version de la suite.

Ces trois exclusions sont volontaires et comptées : 643 cas retenus sur les
~1 500 du dépôt d'Elastic.
