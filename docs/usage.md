# Pondérer la couverture par l'usage réel

Un pourcentage brut ne veut rien dire. « 9,7 % des cas de la suite REST
d'Elastic » met sur le même plan un `bool` + `match` — que neuf applications sur
dix envoient — et un `significant_terms` avec script, que presque personne
n'écrit. Cette page change de dénominateur : au lieu de compter des **cas de
test**, elle compte des **requêtes que quelqu'un a vraiment écrites**, et pose
sur chacune une seule question.

> **Cette requête-là passerait-elle, entièrement ?**

Entièrement, parce qu'une requête supportée à 90 % est une requête qui échoue.
Le verdict est donc par requête, jamais par clause : une seule clause refusée
suffit à la faire tomber.

Les nombres de cette page viennent de [`usage.json`](usage.json), régénéré par
la commande ci-dessous et commité avec le corpus qui l'a produit. Ils bougent
dans la PR qui les fait bouger.

```bash
python3 tests/compat/recolte_usage.py                     # (re)constitue le corpus
python3 tests/compat/ponderation.py --json docs/usage.json
python3 tests/compat/ponderation.py --rejoue http://127.0.0.1:9200 http://127.0.0.1:9201
```

---

## Le corpus : 5 311 requêtes, quatre sources, chacune citable

Rien n'est inventé et rien n'est choisi à la main : chaque requête du corpus
porte l'URL exacte — dépôt, révision, fichier, ligne — d'où elle a été
extraite. [`tests/compat/usage/corpus.jsonl`](../tests/compat/usage/corpus.jsonl) est publié
avec l'étude, une requête par ligne ; [`tests/compat/usage/sources.json`](../tests/compat/usage/sources.json)
en donne les révisions exactes.

| Source | Requêtes | Ce que c'est, et comment elle est ramassée |
|---|---|---|
| `doc` | 3 969 | tous les blocs `[source,console]` de `docs/reference/` d'**Elasticsearch 8.15.0** — les exemples que lit celui qui écrit une requête |
| `rally` | 861 | les **tracks de benchmark d'Elastic** (`elastic/rally-tracks`, Apache-2.0) : chaque opération `search` porte un corps écrit pour mesurer un vrai moteur sur un vrai jeu de données |
| `github` | 338 | la **recherche de code de GitHub**, 184 dépôts publics : des sites d'appel Python (`es.search(index=…)`), dont le corps littéral est extrait par analyse syntaxique |
| `clients` | 143 | les tests et exemples des **clients officiels** — 137 en Python, 4 en Go, 2 en Ruby (voir les biais : un objet littéral JavaScript n'est pas du JSON) |

Le total (5 311) est net de 456 doublons : deux fois exactement le même corps
**dans le même fichier** ne sont qu'un usage — un fichier de doc qui répète son
exemple de départ, un track qui déclare le même corps sous deux noms. Le même
corps dans deux fichiers, lui, compte deux fois : ce sont deux endroits où
quelqu'un l'a écrit.

Une précaution de méthode a coûté une source : sur GitHub, on cherche des
**sites d'appel**, jamais des noms de clause. Chercher `minimum_should_match`
puis compter les `minimum_should_match` mesurerait la question posée, pas
l'usage.

## Ce qu'est « servie entièrement », et comment on le sait

Deux mesures indépendantes, et c'est leur accord qui fait la preuve.

**Le croisement déclaré.** Chaque requête est décomposée en *traits* —
`route:search`, `dsl:bool`, `dsl:match.fuzziness`, `agg:terms.missing`,
`corps:highlight`, `type:keyword` — et chaque trait est rattaché à une capacité
de [`compat.yaml`](../compat.yaml). La requête est servie si aucun de ses traits
n'est refusé. Un trait qu'**aucune capacité ne réclame** compte contre nous,
exactement comme l'`indeterminé` du rapport de conformance : sinon, oublier de
déclarer une capacité ferait monter le taux.

**Le rejeu.** La même requête est envoyée à ferrite **et** à un vrai
Elasticsearch 8.15, et on ne compare que « accepté / refusé » — les deux index
sont vides, il n'y a pas de documents à départager. Une requête qu'ES refuse
aussi sort du dénominateur : elle ne dit rien de ferrite.

Le rejeu a une subtilité qui a failli fausser toute la mesure. Rejoué contre un
index **sans mapping**, il comptait des refus qui ne parlaient que de l'absence
de mapping : sur un champ inconnu, ES rend des seaux vides là où ferrite refuse
l'agrégation — `{"aggs": {"a": {"terms": {"field": "absent"}}}}` rend 200 chez
l'un et 400 chez l'autre, mesuré aux deux serveurs. Toute requête portant une
agrégation aurait donc été comptée refusée pour une raison qui n'était pas la
sienne. Chaque requête est donc rejouée contre un index qui porte **les champs
qu'elle nomme**, du type que sa propre forme suppose (une borne `range` qui
ressemble à une date fait un champ `date`, un `terms` fait un `keyword`…) — 448
mappings différents pour 1 756 recherches. Le même mapping est posé sur les deux
serveurs : une inférence de travers ne peut que sortir la requête du
dénominateur, jamais flatter ferrite.

**Les deux mesures sont d'accord sur 1 363 requêtes sur 1 381** (98,7 %). C'est
ce qui rend le croisement utilisable là où le rejeu ne va pas — les routes, les
mappings, tout ce qui n'est pas un corps de `_search`.

Les dix-sept désaccords vont presque tous dans le même sens — le croisement dit
« servie », le rejeu dit « refusée » — et ils sont **le vrai intérêt du rejeu** :
ils nomment ce que le croisement ne sait pas modéliser. Trois d'entre eux sont
apparus en livrant `stored_fields` : la requête n'était plus bloquée en amont,
le rejeu l'a donc atteinte, et il a trouvé **autre chose** dessous — un
`aggs: {}` vide qu'ES accepte et que ferrite refuse, un `multi_match` sans
`fields` qu'ES résout sur son champ par défaut et que ferrite exige. Cinq de
plus sont apparus de la même façon en livrant les trois paramètres de `sort`
(ils étaient de 11, ils sont de 17), et ils ne parlent pas de tri : un
`require_field_match: false`, un `[query]` qui n'est pas un objet, un `size`
écrit `"1000"`. Débloquer une capacité fait remonter les refus qui étaient
cachés derrière elle ; c'est attendu, et c'est mesuré plutôt que supposé — le
taux d'accord baisse quand la couverture monte, et ce n'est pas une
régression.

## Le résultat : le taux dépend surtout de qui pose la question

| Sous-corpus | Requêtes | Servies entièrement |
|---|---|---|
| **`github` — du code d'application open source** | 338 | **97,0 %** |
| `clients` — tests et exemples des clients officiels | 143 | 83,2 % |
| `doc` — la documentation de référence | 3 969 | 41,6 % |
| `rally` — les tracks de benchmark d'Elastic | 861 | 52,1 % |
| **tout le corpus** | 5 311 | 47,9 % |

Ces quatre nombres ne se contredisent pas, ils mesurent quatre choses
différentes, et l'écart entre eux **est** le résultat :

- une application qui cherche des documents envoie `bool`, `match`, `range`,
  `terms`, un `size`, un `sort` et parfois une agrégation. **Neuf de ces
  requêtes sur dix passent telles quelles** ;
- la documentation de référence consacre **une page par fonctionnalité**, avec au
  moins un exemple chacune : elle sur-représente exactement ce qui est rare. Un
  taux de 40,4 % s'y lit « ferrite couvre un peu plus d'un tiers de la surface
  d'API », ce qui est vrai et sans rapport avec la question précédente ;
- les tracks Rally sont des **bancs d'essai analytiques** : `date_histogram`
  avec `calendar_interval`, `runtime_mappings`, `fields`, `percentiles`. Et le
  track `elastic/logs` rejoue les requêtes de **Kibana**, qui pose
  systématiquement des `runtime_mappings` et des `fields`. 50,6 % — c'était
  17,4 % avant que `fields`, `docvalue_fields` et `stored_fields` ne soient
  livrés, 28,6 % avant les trois paramètres de `sort`, 30,5 % avant que
  `terms` ne sache filtrer ses termes et classer ses seaux sur une
  sous-agrégation, et **32,8 % avant que `date_histogram` ne sache dire « par
  mois »** : ce dernier saut de 13,8 points est le plus gros qu'une seule carte
  ait fait bouger sur ce sous-corpus, et **47,2 % avant que `query_string`
  ne sache lire son mini-langage** ; il dit ce qu'un banc analytique
  demande vraiment — c'est le prix d'entrée pour servir un Kibana, pas celui
  d'une application.

Le corpus n'est pas homogène et il ne prétend pas l'être : la documentation en
fait 74,7 %, et le seul répertoire `elastic/logs` des tracks Rally 8,3 %.
La liste complète des provenances est dans `usage.json` (`concentration`) — une
étude qui ne publie pas ça laisse croire que ses 5 000 requêtes sont 5 000
usages indépendants.

## Ce qui manque, par fréquence d'usage

Le tableau complet est dans `usage.json` (`manques_par_frequence`), avec le
détail par trait et par source. Les dix premiers, et pourquoi la colonne « par
source » compte autant que le total :

| Capacité refusée | Tout le corpus | doc | rally | applications |
|---|---|---|---|---|
| `hors.xpack` — sécurité, ML, ILM, watcher, transform, EQL/SQL… | 18,9 % | 25,2 % | 0,2 % | — |
| `dsl.non_supportees` — `intervals`, `terms_set`, `script`… | 4,4 % | 3,2 % | 12,0 % | — |
| `agg.non_supportees` — `percentiles`, `top_hits`, `filters`… | 6,1 % | 3,0 % | 23,2 % | 0,6 % |
| `recherche.non_supportes` — `search_after`, `pit`, `collapse`… | 3,1 % | 2,0 % | 7,0 % | 1,2 % |
| `type.autres` — `geo_point`, `ip`, `binary`… | 3,0 % | 3,2 % | 3,6 % | — |
| `ingestion.reecriture_en_masse` — `_reindex`, pipelines… | 3,0 % | 3,8 % | 0,8 % | — |
| `type.autres_parametres` — `null_value`, `doc_values`, `store`… | 2,8 % | 2,9 % | 3,7 % | — |
| `hors.cycle_de_vie` — `_close`, `_open`, `_forcemerge`… | 2,6 % | 3,4 % | — | — |
| `hors.snapshots` — `_snapshot/*` | 2,0 % | 2,4 % | 0,8 % | — |
| `hors.cluster_distribue` — `_cluster/state`, `_cluster/reroute`… | 1,8 % | 2,4 % | — | — |

La ligne `agg.date_histogram` (4,9 % du corpus, **25,8 % des tracks Rally** —
elle y était la première) **a disparu de ce tableau** : c'est la carte 13,
faite. Il en reste **une** requête sur 5 311, et elle porte `order`. C'est le
plus gros déplacement qu'une carte ait fait sur ce corpus (259 requêtes citaient
ces paramètres), et il tient à ce que le tableau ne dit pas : sur un banc
analytique, « par mois » n'est pas une option de `date_histogram`, c'en est
l'usage.

La ligne `agg.terms` (2,0 % du corpus, **11,7 % des tracks Rally**) **a disparu
de ce tableau** à son tour : c'est la carte 12, faite. Il en reste 11 requêtes
sur 5 311 — `collect_mode`, `execution_hint`, `script`, `min_doc_count`,
`shard_min_doc_count`, `show_term_doc_count_error`, et **une seule** qui porte
un chemin d'ordre à plusieurs niveaux. Une capacité `partiel` qui tombe de 104
requêtes à 11 est le vrai résultat d'une carte de paramètres : ce n'est pas la
capacité qui manquait, c'étaient ses bords.

La ligne `index.templates` (3,9 % du corpus, 5,1 % de la documentation) **a
disparu de ce tableau** : c'est la carte 20, faite. Ce qui reste de sa famille
est `index.templates_composants` (0,9 %) — les templates de composants et la
simulation, refusés à la pose plutôt qu'appliqués à moitié.

`hors.xpack` en tête est un artefact de la documentation, pas un manque
ressenti : `docs/reference/` documente tout ce qu'Elastic vend, et aucune des
338 requêtes d'application ne l'appelle. C'est précisément ce qu'un taux global
non décomposé aurait caché.

## L'ordre de priorité des cartes, refait à partir des mesures

Les cartes *Search 05* à *21* avaient été posées **à l'intuition**, dans l'ordre
où elles avaient été pensées. Voici le même travail classé par ce qu'il
débloque. Deux colonnes, parce qu'elles ne disent pas la même chose :

- **bloque** : le chantier est *un* des refus qui font tomber la requête ;
- **débloque** : il est *le seul* — la faire passer suffit à faire passer la
  requête. C'est la colonne qui décide, parce qu'un chantier qui ne débloque
  rien tout seul ne change rien pour personne.

> **Ce classement est celui du jour où il a été mesuré.** Deux cartes ont été
> faites depuis. La **20** a fait passer le corpus de **36,3 % à 39,7 %** servi
> entièrement (1 929 → 2 106 requêtes), et la documentation de 34,5 % à 38,8 %.
> Sur le sous-corpus des applications réelles, elle en débloque **zéro** — la
> mesure l'annonçait, et elle avait raison : cette carte vaut pour la surface
> d'API, pas pour débloquer un utilisateur précis.
>
> La **18** a fait passer le corpus de **39,7 % à 41,7 %** (2 106 → 2 215), et
> ce chiffre-là se lit en **deux morceaux**, parce que les mélanger tromperait :
>
> - `fields`, `docvalue_fields` et `stored_fields` livrés en débloquent **30**
>   (39,7 % → 40,2 %), dont **4 sur le sous-corpus des applications réelles** —
>   exactement ce que la ligne du tableau annonçait ;
> - accepter l'objet **vide** de `script_fields` / `runtime_mappings` en
>   débloque **79 de plus** (40,2 % → 41,7 %). C'est une vraie compatibilité —
>   ES rend la même réponse avec ou sans, et 774 requêtes du corpus l'envoient
>   sous cette forme — mais c'est **un artefact de gabarit** : les tracks Rally
>   laissent le paramètre vide quand il n'est pas rempli. Ça ne prouve rien sur
>   `script_fields`, qui reste ❌.
>
> La **19** a fait passer le corpus de **41,7 % à 42,1 %** (2 215 → 2 237), et
> le sous-corpus des applications réelles de **90,8 % à 93,2 %** — c'est le plus
> gros saut qu'une carte lui ait fait faire, et c'était l'annonce du tableau.
> Mais **22 requêtes débloquées, pas 75**, et l'écart s'explique en deux temps :
>
> - la ligne 3 comptait `_delete_by_query`, `_update_by_query` **et
>   `_reindex`** ensemble, parce que `compat.yaml` n'en faisait qu'une seule
>   capacité. `_reindex` reste hors périmètre : ses **42** requêtes n'ont pas
>   bougé, et la ligne du tableau ne le disait pas — trois routes derrière un
>   seul identifiant se lisent comme une seule ;
> - sur les 35 requêtes restantes, **13 demandent un paramètre refusé** :
>   `script` (6, c'est Painless), `slice` (4), `slices` (2), `pipeline` (2),
>   `routing` (2). Ces refus-là n'étaient pas comptés avant, parce que le
>   croisement ne regardait que la **route** d'une commande par requête, jamais
>   ses paramètres — un `_delete_by_query` en cinq tranches parallèles comptait
>   donc « servie ». `ponderation.py` lit maintenant les deux, corps et query
>   string : c'est la même règle que partout ailleurs, et elle rend le
>   dénominateur un peu moins flatteur.
>
> La **32** — `search_analyzer`, `copy_to`, `store` — a fait passer le corpus
> de **42,1 % à 42,5 %** (2 237 → 2 259), et le sous-corpus des applications
> réelles de 93,2 % à **93,5 %**. Mais le détail compte plus que le total : les
> trois paramètres de mapping n'en débloquent que **8**, et c'est attendu — un
> corpus fait de **corps de requêtes** ne pèse presque rien sur des paramètres
> de **mapping**, exactement comme pour l'`index: true` de Gitea. Les 14 autres
> viennent de ce que la carte a trouvé **derrière** eux : le `missing` d'une
> agrégation `terms`, et l'écriture `mustNot`.
>
> Ce que les trois paramètres débloquent vraiment ne se mesure pas ici : il se
> mesure dans [`application.md`](application.md), où le compte est passé de 0 à
> 83 sur les tests de backend de Wagtail. Deux dénominateurs, deux vérités, et
> c'est pour ça qu'il en faut plus d'un.
>
> La **05** — `highlight` — a fait passer le corpus de **42,5 % à 42,8 %**
> (2 259 → 2 272), et la prévision de la ligne 4 disait **18**. Elle en
> débloque **13**, et l'écart vient de la mesure elle-même : la ligne comptait
> le trait `corps:highlight` en bloc, comme si la clef entière était servie ou
> refusée. `ponderation.py` lit maintenant **chaque réglage** du bloc
> (`corps:highlight.type`, `corps:highlight.order`…), et 11 des 102 requêtes
> qui citent `highlight` demandent un réglage refusé — `order` (8), `type` (6),
> `matched_fields` (3), `fragmenter` (2), `highlight_query` (1). Sans cette
> lecture, un `type: fvh` aurait compté « servi », et le dénominateur aurait
> été plus flatteur que la vérité. C'est le même geste que pour la ligne 3, un
> cran plus bas.
>
> La **38** — les manques que seule la suite d'OpenSearch voit — n'a fait bouger
> le total que de **42,8 % à 42,9 %** (2 272 → 2 281), et c'est le sous-corpus
> qui est intéressant : le code d'application open source passe de **93,8 % à
> 96,2 %**, soit huit requêtes sur les vingt-et-une qu'il refusait encore. Les
> neuf débloquées le sont toutes par le même paramètre, `?timeout=` sur
> `_search` — accepté et sans objet, comme `preference`. Deux remarques valent
> plus que le chiffre. D'abord, il ne venait d'aucune des deux suites de
> conformance mais de la **suite de tests du client go** : un corpus de corps de
> requêtes le voyait (94 requêtes le posent), sans qu'aucune source ne le
> désigne comme le prochain manque à combler. Ensuite, `timeout` sortait de
> `recherche.non_supportes` : sans le rattacher à sa nouvelle capacité, les 94
> requêtes seraient tombées dans « aucune capacité ne les réclame », qui compte
> **contre** nous — c'est le garde-fou qui rend un dénominateur non choisi, et
> il a servi ici.
>
> La **09** — `missing`, `mode`, `unmapped_type` sur un `sort` — a fait passer
> le corpus de **42,9 % à 43,2 %** (2 277 → 2 297), et c'est encore le
> sous-corpus qui parle : les tracks Rally passent de **28,6 % à 30,5 %**, parce
> que 79 de leurs requêtes posent `unmapped_type` sur un tri (elles interrogent
> plusieurs index dont tous ne mappent pas le champ trié — exactement la
> situation que le paramètre existe pour couvrir). Le code d'application, lui,
> ne bouge pas d'une requête : il trie sur ses propres champs, dans son propre
> index.
>
> Et il y a une seconde moitié qui n'est pas un gain, mais une **honnêteté
> retrouvée**, et c'est la plus instructive. Le chiffre d'avant, 42,9 %, était
> flatté : `recherche.sort` ne déclarait **aucun** paramètre supporté, et
> `ponderation.py` ne rend `indéterminé` un paramètre non déclaré que si la
> capacité en déclare au moins un. Cinq paramètres de tri (`unit`,
> `distance_type`, `pin.location`, `ignore_unmapped`, `type` — tous venus de
> `_geo_distance` et de `_script`, deux clés que ferrite refuse) comptaient donc
> **servis** par simple silence. Les déclarer coûte quatre requêtes, et les deux
> mesures ci-dessus sont prises à déclaration corrigée des deux côtés : 2 277
> avant, 2 297 après. Le garde-fou de la ligne précédente a une condition qu'on
> ne lit pas dans son énoncé — il ne s'arme que sur une capacité qui a commencé
> à se déclarer.
>
> La **12** — `include` / `exclude` et l'ordre par sous-agrégation sur un
> `terms` — a fait passer le corpus de **43,2 % à 43,6 %** (2 297 → 2 316), et
> les tracks Rally de **30,5 % à 32,8 %**. La ligne du tableau annonçait 14 ;
> la mesure en donne 19, et l'écart s'explique de la même façon que plus haut :
> la ligne avait été calculée en supposant l'ordre par sous-agrégation servi
> **entièrement**, alors que ce qui reste refusé (le chemin à plusieurs
> niveaux, la forme partitionnée, un filtre posé en même temps qu'un `missing`)
> est plus étroit que ce que la carte prévoyait. Une prévision de déblocage se
> vérifie après coup, elle ne se recopie pas.
>
> Les **125** de la ligne 2 supposaient les cinq faits, `runtime_mappings` et
> `script_fields` compris : ils demandent Painless, et la mesure a servi à
> décider de ne pas les faire (voir plus bas). Les autres lignes n'ont pas été
> recalculées ; elles se refont avec la recette qui suit.
>
> La **13** — `calendar_interval`, `time_zone`, `format`, et `time_zone` sur la
> borne d'un `range` — a fait passer le corpus de **43,6 % à 45,9 %**
> (2 316 → 2 438), et les tracks Rally de **32,8 % à 46,6 %**. La ligne du
> tableau annonçait 11 ; la mesure en donne **122**, soit onze fois plus, et
> c'est le plus gros écart entre une prévision et sa mesure de tout ce tableau.
> La raison est écrite dans la colonne « bloque » : 259 requêtes citent ces
> paramètres, et la prévision comptait celles que *cette carte seule* débloquait
> — donc celles dont **tout le reste** passait déjà. Entre-temps, six cartes ont
> livré ce qui bloquait à côté (`fields`, les paramètres de `sort`, les filtres
> de `terms`, `_delete_by_query`…), et le calendrier était devenu la **dernière**
> clause manquante de cent onze requêtes de plus. Une prévision de déblocage
> vieillit à mesure que ses voisines sont faites : elle se remesure avant d'être
> citée.

| Rang mesuré | Carte | bloque | débloque | débloque (applications) |
|---|---|---|---|---|
| 1 | **20** — `_validate`, `field_caps`, `_stats`, templates, `PUT _settings` — **faite** | 262 | **239** | 0 |
| 2 | **18** — `fields`, `docvalue_fields`, `stored_fields` — **faite** (+ `runtime_mappings`, `script_fields`, mesurés puis écartés) | 504 | **125** | 4 |
| 3 | **19** — `_delete_by_query`, `_update_by_query` | 77 | **75** | **10** |
| 4 | **05** — `highlight` — **faite** | 102 | **13** | 0 |
| 5 | **09** — `sort` : `missing`, `mode`, `unmapped_type` — **faite** | 94 | **20** | 0 |
| 6 | **12** — `terms` : `include`, `exclude`, ordre par sous-agrégation — **faite** | 103 | **19** | 0 |
| 7 | **21** — les analyzers de langue | 22 | 14 | 0 |
| 8 | **13** — `date_histogram` : `calendar_interval`, `time_zone` — **faite** | 259 | **122** | 1 |
| 9 | **15** — `function_score`, `boosting` | 45 | 7 | 0 |
| 10 | **16** — `explain`, `_explain`, `matched_queries` | 7 | 7 | 1 |
| 11 | **17** — `collapse`, `post_filter`, `min_score` | 11 | 7 | 2 |
| 12 | **07** — les paramètres manquants de `match` | 8 | 5 | 1 |
| 13 | **08** — `search_after`, point-in-time | 11 | 5 | 0 |
| 14 | **10** — `_msearch` | 1 | 1 | 0 |
| 15 | **06** — `query_string`, `simple_query_string` | 90 | **0** | 0 |
| 16 | **11** — agrégations `filters`, `date_range` | 13 | **0** | 0 |
| 17 | **14** — `percentiles`, `extended_stats`, `top_hits` | 94 | **0** | 0 |

Le calcul se refait : chaque ligne est l'ensemble des traits ci-dessous,
comptés sur `tests/compat/usage/verdicts.jsonl` (`manques[].trait`).

| Carte | Traits comptés |
|---|---|
| 05 | `corps:highlight`, `corps:highlight.*` |
| 06 | `dsl:query_string`, `dsl:simple_query_string` |
| 07 | `dsl:match.{fuzziness, minimum_should_match, analyzer, zero_terms_query, prefix_length, auto_generate_synonyms_phrase_query, fuzzy_transpositions, max_expansions}` |
| 08 | `corps:search_after`, `corps:pit` |
| 09 | `tri:{missing, mode, nested, unmapped_type, script, geo, format, numeric_type}` |
| 10 | `route:msearch` |
| 11 | `agg:filters`, `agg:date_range`, `agg:ip_range` |
| 12 | `agg:terms.{include, exclude, order=sous_agregation, missing, script, collect_mode, execution_hint, shard_min_doc_count, show_term_doc_count_error}` |
| 13 | `agg:date_histogram.{calendar_interval, time_zone, format, order}` |
| 14 | `agg:{percentiles, extended_stats, top_hits, percentile_ranks, median_absolute_deviation, boxplot, top_metrics}` |
| 15 | `dsl:{function_score, boosting, script_score, rank_feature, distance_feature, pinned}` |
| 16 | `corps:explain`, `route:explain`, `dsl:*._name` |
| 17 | `corps:collapse`, `corps:post_filter`, `corps:min_score` |
| 18 | `corps:{fields, docvalue_fields, stored_fields, script_fields, runtime_mappings}` |
| 19 | `route:delete_by_query`, `route:update_by_query`, `route:reindex` |
| 20 | `route:{indices.validate_query, field_caps, indices.stats, indices.put_settings, indices.rollover}` et les routes de templates (`indices.*_template`, `cluster.*_component_template`) |
| 21 | `analyzer:<langue>` pour toute langue autre que `english` et `french` |

### L'écart avec l'intuition, qui est lui-même un résultat

- **La carte 20 passe de l'avant-dernière place à la première.** Elle
  s'appelait « les petites routes qui bloquent un client » et ressemblait à un
  fourre-tout ; c'est le chantier qui débloque le plus de requêtes du corpus,
  parce que **les templates** sont partout dans la documentation et dans tout
  script d'initialisation d'index. Le mot « petites » était le mauvais mot.

  Vérification après coup, qui est le seul vrai test d'une prédiction : la carte
  faite, le corpus passe de 36,3 % à **39,7 %**, soit **177 requêtes** de plus
  servies entièrement. La prédiction disait 239 ; l'écart est ce qui reste
  refusé *à côté* dans les mêmes requêtes — les templates de composants
  (`composed_of`, `_component_template`, 0,9 % à eux seuls) et la simulation,
  qui n'étaient pas séparés de la carte au moment du classement.
- **La carte 19 (`_delete_by_query`) est le premier blocage des applications
  réelles** — 10 des 481 requêtes d'application et de client, plus que tout le
  reste. Elle était 15e sur 17. Un code qui gère des données fait des
  suppressions en masse ; un code d'exemple, non.
- **La carte 10 (`_msearch`) est dernière avec une requête.** `compat.yaml` la
  décrit comme « la plus regrettée » de sa famille. Le corpus dit le contraire —
  avec un biais qu'il faut nommer : `_msearch` est une optimisation de
  *transport* (six facettes en un appel), pas une fonctionnalité de recherche ;
  ni la documentation ni un site d'appel Python ne la montrent souvent. La
  mesure ne dit pas qu'elle est inutile, elle dit que **rien dans ce corpus ne
  la réclame**, ce qui n'est pas la même chose et suffit à la déclasser.
- **Trois cartes débloquent zéro requête à elles seules** (06, 11, 14) : elles
  bloquent — `query_string` fait tomber 90 requêtes — mais toujours en compagnie
  d'un autre refus. Les faire ne fera passer aucune requête de plus tant que le
  reste n'est pas fait. C'est un argument pour les **grouper**, pas pour les
  abandonner.

  Et c'est ce que la suite a mesuré, ce qui est la meilleure chose qui pouvait
  arriver à cette ligne : `query_string` livrée, le corpus passe de **46,3 % à
  47,4 %** (+58 requêtes) et les tracks Rally de **47,2 % à 50,6 %**. Le
  « zéro » n'était pas faux — il était **daté**. Les compagnons qui tombaient
  avec elle (`fields`, les paramètres de `sort`, `date_histogram`, `highlight`,
  `function_score`, `min_score`) ont été livrés entre-temps, et c'est
  exactement ce que « grouper » voulait dire. Un classement d'impact ne vaut
  que pour l'état du périmètre au jour où il est calculé, et il se recalcule.

  La carte 17 (`collapse`, `post_filter`) en est le contre-exemple utile, et
  il vaut la peine d'être écrit : elle ne déplace le corpus que de **+7
  requêtes**, parce que les dix requêtes qui les posent sont
  presque toutes des exemples de documentation qui butent **aussi** sur autre
  chose. Ce n'est pas la mesure de ce que la carte vaut : les deux paramètres
  sont la mécanique d'une page à facettes et d'un catalogue dédoublonné, et
  une page à facettes ne s'écrit pas en dix requêtes de documentation. Le
  corpus dit ce que les gens **envoient**, pas ce qu'ils ne peuvent pas
  écrire ; c'est sa limite, et elle se voit ici mieux qu'ailleurs.
- **La carte 13 (`date_histogram`) bloque 259 requêtes et n'en débloque que 11.**
  Le tableau de bord qui l'envoie envoie aussi `runtime_mappings` et `fields`
  (carte 18). Les deux ensemble sont un lot ; l'une sans l'autre ne sert pas un
  Kibana.
- **La carte 18 contenait un chantier qu'il ne fallait pas faire, et c'est le
  corpus qui l'a dit.** Sa ligne agrège cinq paramètres, dont
  `runtime_mappings` — qui à lui seul *bloque* 439 requêtes, le plus gros
  contributeur des 504. L'intuition dit donc « il faut le faire ». La mesure,
  demandée avant de décider, dit l'inverse en deux chiffres :

  - sur les **444** requêtes du corpus qui portent `runtime_mappings`, **425
    l'envoient vide** (`{}`) — des gabarits de tracks Rally dont le paramètre
    n'est pas rempli. Elles ne demandent rien ;
  - sur les **19** qui ne sont pas vides, **18 portent un script Painless**, et
    la dernière est un champ `lookup`. Il n'existe pas, dans ce corpus, de
    `runtime_mappings` sans script — donc pas de version « facile » à livrer.

  Seul, `runtime_mappings` débloque **6** requêtes, et **3** en sont les seules
  bloquées. Painless est hors périmètre déclaré ; il reste ❌, et ce qui a été
  livré à sa place est l'acceptation de l'objet vide, qui coûte trois lignes et
  vaut 79 requêtes. **La bonne façon d'utiliser ce corpus n'est pas seulement de
  choisir quoi faire en premier : c'est aussi de démontrer qu'un chantier gros
  et cher ne vaut pas la peine.**

Ordre proposé, une fois ces deux lectures superposées : **19** (les applications
d'abord), **20**, puis le lot **18 + 13** (servir un tableau de bord), puis
**05**, **09**, **12**, **21**, puis le reste dans l'ordre du tableau, en
groupant **06 + 14 + 11**.

## Ce que la mesure a trouvé en chemin

Une étude qui ne trouve que ce qu'elle cherchait n'a rien mesuré.

**Un échec silencieux, corrigé dans la même PR.** Le rejeu a montré deux
requêtes que ferrite acceptait alors que `compat.yaml` déclare les refuser. La
cause est la même que celle de la recherche sans index, un cran plus bas :
`range`, `term`, `terms` et `regexp` résolvaient leur champ **avant** de lire
leurs paramètres. Sur un champ jamais mappé — toléré par
`allow_unmapped_fields`, le défaut d'ES — la clause court-circuitait donc avant
son propre refus : `time_zone`, `relation`, un *terms lookup*,
`case_insensitive` et les opérateurs Lucene que ferrite ne sait pas construire
(`~`, `&`, `<n-m>`, `#`) passaient en silence. Les paramètres se lisent
maintenant d'abord, et le harnais de compat porte le scénario qui l'exige.

**Un cinquième du corpus tombait dans un trou de la déclaration.** 1 002
requêtes visaient des routes qu'**aucune capacité de `compat.yaml` ne
réclamait** : `_security`, `_ml`, `_ilm`, `_watcher`, `_transform`, `_eql`,
`_sql`, `_inference`… Le périmètre déclaré ne disait nulle part que ferrite ne
fait pas ce qu'Elastic vend autour du moteur. Elles étaient donc comptées comme
indéterminées — contre nous, ce qui est la bonne règle, mais un chiffre qu'on
subit. `hors.xpack` le déclare maintenant, et avec lui une vingtaine de paramètres
que le rejeu a vus refusés sans qu'une ligne les nomme (`missing` et
`collect_mode` sur `terms`, `time_zone` et `format` sur `date_histogram`,
`runtime_mappings` et `script_fields` dans le corps, `_name` sur toutes les
clauses…). Il ne reste, sur les 5 311 requêtes du corpus, **deux** traits
qu'aucune capacité ne réclame.

**Deux divergences de tolérance, mesurées, pas corrigées.** ES accepte des
choses que ferrite refuse, et ce sont de vraies requêtes qui les portent :

| Ce qu'un client envoie | ES 8.15 | ferrite |
|---|---|---|
| `{"size": "1000"}` — un nombre écrit en chaîne (`from`, `size`, `boost`) | 200, la valeur est convertie | 400 |
| un `null` au milieu d'un tableau de clauses d'un `bool` (Kibana en émet) | 200, la clause est sautée — mais 400 si le `null` est **en dernier** | 400 |

La règle du `null` n'était devinable ni d'un côté ni de l'autre : elle vient de
poser les deux formes aux deux serveurs. Aucune des deux n'est un échec
silencieux — ferrite refuse bruyamment — donc aucune n'est corrigée ici : ce
sont des décisions à prendre, pas des bugs à réparer, et elles sont maintenant
chiffrées.

## Les biais, en toutes lettres

Une étude qui annonce ses biais résiste ; une qui les cache ne résiste pas au
premier lecteur attentif.

- **La documentation sur-représente le rare.** Une page par fonctionnalité, au
  moins un exemple par page : dans ce sous-corpus, `significant_terms` pèse
  autant que `match`. C'est pour ça que le taux global (39,7 %) est un chiffre
  de couverture d'API, pas un chiffre d'usage — et c'est pour ça que les
  colonnes par source sont partout dans cette page.
- **Les tracks Rally sur-représentent le log, la géo et l'analytique**, et
  incluent les requêtes de Kibana : tout ce que cette application-là fait
  systématiquement hérite de son poids.
- **Le code open source ne représente pas les usages internes.** Ce qu'on trouve
  sur GitHub, c'est de l'outillage, des projets de recherche, des intégrations —
  rarement le back-end d'un commerce en ligne, jamais le code d'un client
  d'Elastic.
- **La recherche de code n'est pas reproductible à l'identique.** Ses résultats
  changent d'un jour à l'autre ; ce qui est reproductible, c'est le **corpus
  publié**, avec la date de collecte et les requêtes de recherche exactes
  (`usage.json`, `corpus.sources.github`).
- **Un seul langage pour le code d'application.** Les corps ne sont extraits par
  analyse syntaxique que des sites d'appel **Python** : un objet littéral
  JavaScript ou une structure Go ne sont ramassés que s'ils sont du JSON valide.
  Java, C#, PHP et Ruby sont un angle mort déclaré.
- **Un corps littéral seulement.** Une requête construite par le programme
  (un `dict` assemblé au fil des `if`) n'est pas lisible sans exécuter le code,
  et l'inventer serait pire que de la manquer. Le corpus penche donc vers les
  requêtes simples — dans le sens qui **flatte** ferrite. C'est le biais le plus
  gênant de cette étude, et il n'a pas de correctif à ce coût-là.
- **Le rejeu ne mesure que la validation du corps.** Deux serveurs vides : rien
  n'y prouve que les documents rendus seraient les mêmes. C'est le travail de
  [`diff_relevance.py`](../tests/compat/diff_relevance.py) et des autres
  comparateurs, sur un corpus de documents, lui.
- **Le sous-corpus « applications » est petit** : 481 requêtes, 184 dépôts. Ses
  pourcentages ont trois chiffres significatifs de trop ; c'est un ordre de
  grandeur, pas une mesure fine.
- **Un corpus de requêtes ne voit pas le premier appel d'une application.**
  Celui-là n'est pas une recherche : c'est un `PUT /{index}` avec un mapping
  écrit par un générateur. Brancher une vraie application
  ([`application.md`](application.md)) a trouvé un refus qui bloquait Gitea
  **entièrement** au démarrage — `"index": true` sur chacun de ses champs, le
  défaut d'ES — sans que ce corpus bouge d'une requête (42,1 % avant, 42,1 %
  après) ni la suite REST d'Elastic d'un cas (356 échecs avant et après). Un
  corpus fait de corps de recherche pèse presque tout son poids sur `_search`,
  et ce biais-là ne se lit pas dans la colonne des sources.

## Les poids dans `compat.yaml`

Chaque capacité de [`compat.yaml`](../compat.yaml) porte un `poids` : la part
des requêtes du corpus qui l'exercent, en pour-cent à une décimale. Il ne
s'écrit pas à la main.

```bash
python3 tests/compat/ponderation.py --poids     # les écrit depuis la mesure
python3 tests/compat/ponderation.py --verifie   # ce que lance la CI
```

Sur 180 capacités, **125 portent un poids** ; les 55 autres gardent `null`, et
c'est voulu : le corpus ne sait pas les exercer. La forme d'une réponse, le
scoring, le mapping dynamique, la création d'index à l'écriture ne se lisent pas
dans une requête. Mettre `0.0` là où on n'a pas mesuré ferait passer un trou de
la mesure pour une absence d'usage — la confusion exacte que cette page existe
pour éviter.
