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
| réussis | **44** | **537** |
| refusés explicitement (hors périmètre) | 333 | 0 |
| sautés (borne de version, fonctionnalité du runner) | 97 | 103 |
| **échecs** | **169** | **3** |
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
manques francs**, tous deux corrigés depuis — c'est ce qui fait passer le compte
à 44 réussis et 169 échecs :

| Combien | Ce que c'était | Corrigé |
|---|---|---|
| **58** | `no such index [test_1]` — les tests indexent sans créer l'index | ✅ l'écriture crée l'index à la volée (`index`, `create`, `update`, `_bulk`), comme ES. La **lecture** et la **suppression** rendent toujours 404, comme ES aussi |
| **34** | `Invalid index name [_refresh]`, `[_all]`, `[_mapping]` | ✅ `POST /_refresh`, `GET /_mapping`, et `_all` / `*` sur les routes administratives |
| **3** | `{"type": "object"}` sans `properties` refusé | ✅ accepté, l'objet ne déclare rien et ses champs viendront des documents |

Ce qui reste dans les 169 :

| Combien | Ce que c'est | Verdict |
|---|---|---|
| **~40** | `unknown query [intervals]`, `unrecognized parameter: [version]`… | des refus **réels**, mais dont le type d'erreur imite celui d'Elasticsearch (`parsing_exception`) au lieu de porter le marqueur `not_implemented_in_ferrite_exception`. Compter juste demanderait de mentir sur le type ; on préfère la fidélité et une colonne moins flatteuse |
| **~18** | `include_type_name`, `_type` attendu dans la réponse | la suite est celle de la 7.10 ; ces cas testent ce que la **8.x a supprimé**. Un vrai ES 8 y échoue aussi |
| **7** | `_close` / `_open` | hors périmètre déclaré |
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
