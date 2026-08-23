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

## Le dénominateur n'est pas choisi

Ce runner a longtemps porté une liste blanche de 22 domaines, choisis pour être
ceux que ferrite prétend savoir faire. L'intention était bonne et le résultat
mauvais : **un dénominateur qu'on choisit soi-même ne prouve rien.** Même
documenté, « il n'a gardé que les suites qui l'arrangeaient » est la première
chose qu'on lira. Et une suite écartée d'avance ne peut rien apprendre : `scroll`,
les alias et `indices.get_field_mapping` exercent des capacités que ferrite
déclare tenir, et n'étaient jamais lancés.

Les 107 domaines de la suite sont donc **tous** joués. Le tri d'un cas — hors
périmètre, non implémenté, échec — se calcule sur ce que le serveur répond ; il
ne s'obtient pas en n'envoyant pas la question. Un domaine entièrement hors
périmètre (snapshots, ILM, scripts, cluster distribué) apparaît dans le rapport
avec ses cas rangés en `refus` ou en `echec` : visible plutôt qu'absent.

Restent deux exclusions, et elles sont **comptées** dans le rapport
(`mesure.exclusions`), pas seulement décrites :

| Exclusion | Pourquoi |
|---|---|
| les fichiers `*_with_types.yml` | ils testent l'API typée (`/{index}/{type}/{id}`), supprimée en 8.x. Jamais ouverts, donc hors du dénominateur |
| les cas que le vocabulaire du runner ne sait pas jouer | un verbe ou une `feature` du runner officiel qu'on n'implémente pas. Ouverts, comptés en `sautes` |

Une troisième ligne y figure pour achever de décomposer la colonne `sautes` :
les cas que la suite borne elle-même par `skip: {version}`. Ce n'est pas une
exclusion de notre fait.

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

Les totaux et les trois taux, sans rien installer :

```bash
python3 - <<'PY'
import json
r = json.load(open("docs/conformance.json"))
print(r["mesure"]["date"], r["mesure"]["cible"]["version_annoncee"])
print(r["totaux"])
print(r["perimetre"]["regressions"], "regressions,",
      r["perimetre"]["couts_perimetre"], "couts de perimetre")
for nom, t in r["taux"].items():
    print(f'{nom}: {t["numerateur"]}/{t["denominateur"]}')
PY
```

## Quatre catégories, trois taux

Les catégories ne mesurent pas la même chose, et c'est la distinction qui
compte :

| Catégorie | Ce qu'elle veut dire |
|---|---|
| `reussi` | ferrite répond comme Elasticsearch |
| `refus` | ferrite répond « je ne sais pas faire » (`not_implemented_in_ferrite_exception`) : c'est le contrat, pas un bug |
| `saute` | le cas ne mesure pas la cible — borne de version de la suite, ou verbe que ce runner n'implémente pas |
| `echec` | ferrite répond **autre chose** qu'Elasticsearch : ce sont les seuls vrais écarts |

D'où trois taux, qui ne répondent pas à la même question :

- **fidélité** = `reussis / (reussis + echecs)`. Un pis-aller, et il faut dire
  pourquoi : une partie des `echec` sont en réalité des refus, mais dont le type
  d'erreur imite Elasticsearch (`parsing_exception` sur `unknown query
  [intervals]`, ou `illegal_argument_exception` sur un paramètre non reconnu) au
  lieu de porter le marqueur `not_implemented_in_ferrite_exception`. Ils
  gonflent donc le dénominateur alors qu'ils sont hors périmètre. Compter
  « juste » en trichant sur le type d'erreur serait pire : on préfère la
  fidélité du message.
- **fidélité dans le périmètre déclaré** = `reussis / (reussis + regressions +
  indetermines)`. Le même, mais chaque échec est d'abord **croisé avec
  [`compat.yaml`](../compat.yaml)** — voir ci-dessous. C'est le taux qui dit si
  **ce qu'on annonce est juste**.
- **couverture brute** = `reussis / total`. Quelle part de la suite d'Elastic
  passe, périmètre non déclaré compris. C'est le taux qui dit **quelle part
  d'Elasticsearch on couvre**.

Confondre les deux extrêmes, c'est se flatter (ne citer que le deuxième) ou se
punir (ne citer que le dernier). Le JSON porte les trois, chacun avec son
numérateur et son dénominateur, pour qu'ils se recalculent.

## Un échec sur quoi ? Le croisement avec le périmètre déclaré

Un échec sur `_snapshot` et un échec sur `_search` pesaient jusqu'ici pareil.
Ce n'est pourtant pas le même événement : le premier est le prix d'un périmètre
qu'on a **choisi**, le second un défaut de ce qu'on **annonce**. Depuis que le
périmètre est une donnée ([`compat.yaml`](../compat.yaml)), le runner peut
trancher, cas par cas :

| Verdict | Ce que c'est |
|---|---|
| `regression` | le cas exerce une capacité déclarée **supportée** — c'est un vrai écart, il compte |
| `cout_perimetre` | le cas exerce une capacité déclarée **refusée** — attendu, c'est le prix affiché du périmètre |
| `indetermine` | aucune capacité ne réclame ce cas. Il compte **contre nous**, comme une régression |

Le troisième verdict est le garde-fou : si un cas non rattaché sortait du
dénominateur, oublier de déclarer une capacité ferait monter le taux — le
fichier deviendrait un outil pour se flatter, exactement ce que la liste blanche
de 22 domaines faisait avant lui.

Le rattachement est expliqué dans [`../tests/compat/perimetre.py`](../tests/compat/perimetre.py),
qui se lance seul pour éprouver une attribution :

```bash
python3 tests/compat/perimetre.py                      # l'index, tel qu'il est lu
python3 tests/compat/perimetre.py search "unknown query [intervals] ..."
```

Il repose sur trois signaux, et aucun n'est un jugement porté ici : l'API
appelée par le `do` qui a échoué, le message d'erreur du serveur, et — pour
l'API typée (`/{index}/{type}/{id}`, disparue en 8.x) — le fait que le cas ait
demandé une URL qui la porte. Le rapport garde ces éléments par cas (`api`,
`capacite`, `perimetre`, `mise_en_place`), donc chaque verdict se vérifie.

Un cas peut changer de verdict d'une mesure à l'autre sans que le nombre
d'échecs bouge : `indices.stats/11_metric.yml::Metric - multi` échoue tantôt sur
sa mise en place (« no such index »), tantôt sur `_stats`, et le verdict suit
l'endroit où il bute. Le partage régressions / coûts de périmètre se lit donc à
un cas près ; le cliquet, lui, porte sur les catégories, qui ne bougent pas.

### Ce que le croisement a trouvé du premier coup

Trois écarts entre ce que `docs/compat.md` déclarait et ce que ferrite fait,
tous invisibles tant que les 400 échecs étaient anonymes :

| Ce que la doc disait | Ce que la mesure dit |
|---|---|
| `GET /{index}/_settings` ✅ | 15 cas y échouent : `GET /_settings` sans index rend `invalid_index_name_exception`, `/{index}/_settings/{nom}` n'existe pas, `local` est refusé. La capacité est **partielle**, elle est déclarée telle — les trois manques ont été écrits depuis, et ces cas passent |
| `_cluster/health` 🟡, seul `level` refusé | tous les `wait_for_*` d'attente de shards sont refusés eux aussi (9 cas), ce qui est juste sur un mono-nœud — mais n'était écrit nulle part |
| `_cat/indices` / `_cat/health` 🟡 | `help` et `ts` sont refusés en plus de `h` et `s` |

Et un vrai défaut, celui-là à corriger — **corrigé depuis** : *une recherche qui
ne vise aucun index ne validait pas son corps.* `POST /_search` sur un cluster
vide (ou un motif qui ne correspond à rien) rendait 200 avec une clause que
ferrite refuse partout ailleurs — `{"aggs": {"a": {"significant_terms": …}}}`,
`{"query": {"intervals": …}}` — parce que la traduction du Query DSL se fait
index par index, donc zéro index voulait dire zéro traduction. C'était le seul
endroit connu où la règle « jamais d'échec silencieux » ne tenait pas, et il a
été trouvé par un cas d'agrégation d'Elastic qui attendait une erreur et a reçu
un 200. Le corps est maintenant traduit contre un **schéma vide** avant qu'on
conclue qu'il n'y a rien à chercher ; seuls restent suspendus les verdicts
qu'aucun mapping ne peut prononcer, qu'ES diffère lui aussi à l'exécution d'un
shard. Deux petits frères de la même famille, trouvés dans la foulée et
corrigés avec lui : `include_defaults` et `flat_settings` étaient **acceptés et
ignorés** par `GET /{index}/_settings` (ES rend un bloc `defaults`, ou des clés
aplaties) ; ils sont désormais refusés, sur les réglages d'index comme sur ceux
du cluster.

Ces trois-là comptaient en **régressions** dans le rapport — c'est bien ce
qu'ils étaient, et c'est ainsi qu'ils ont été trouvés. Ils en sont sortis avec
le correctif : quatre cas quittent cette colonne et sept passent d'« échec » à
« refusé » (les comptes exacts sont dans [`conformance.json`](conformance.json),
pas ici). La frontière avec ES sur un serveur vide est désormais tenue par
[`tests/compat/sonde_vide.py`](../tests/compat/sonde_vide.py), qui refuse de
tourner si l'un des deux serveurs n'est pas vide.

### Le même geste, contre le conteneur de référence, pour trois minutes

Un cas qui échoue contre ferrite n'échoue pas forcément **parce que** c'est
ferrite. La suite est figée à la 7.10.2 ; ce qu'elle décrit est parfois ce
qu'ES faisait alors et ne fait plus. Le runner accepte une liste de domaines,
donc la question se tranche en une commande :

```bash
python3 tests/compat/conformance_es.py http://localhost:9201 \
  --suites field_caps,indices.validate_query,indices.stats,indices.put_settings
```

Sur les domaines des cinq routes ajoutées par la carte 20, un vrai
**Elasticsearch 8.15** rend 106/118 : les douze échecs qui restent chez lui sont
l'API typée (9 cas, un `{type}` dans l'URL), et — celui qui comptait —
`field_caps/30_filter.yml::Field caps with index filter`. Il pose une borne
`gte: 2019` sur un champ `date` : la 7.10 lisait ce nombre nu comme une année,
la 8.x comme un `epoch_millis`. **ferrite rend la même chose qu'ES 8.15**, donc
ce cas ne mesure pas un manque de ferrite — il mesure un coût de migration
7→8, comme le probe 7.x en mesure d'autres. Le rapport le compte pourtant en
régression : le runner n'a pas de troisième catégorie pour ça, et inventer une
exception au cas par cas serait le début d'un dénominateur choisi.

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
  -e node.attr.testattr=test -e path.repo=/tmp/repo \
  docker.elastic.co/elasticsearch/elasticsearch:7.10.2
docker exec -u 0 es-ref-7102 sh -c 'mkdir -p /tmp/repo && chown 1000:0 /tmp/repo'
python3 tests/compat/conformance_es.py http://localhost:9202 \
  --json docs/conformance-es7102.json
```

Les deux réglages ne sont pas décoratifs : la suite d'Elastic est écrite pour
**le cluster de test d'Elastic**, qui démarre avec `node.attr.testattr=test` et
un `path.repo`. Sans eux, `cat.nodeattrs`, `cluster.put_settings` et les 22 cas
de snapshot échouent contre un vrai ES — et on lirait ces échecs comme des
défauts du runner.

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

### Passer de 22 à 107 domaines a coûté quatre corrections de plus

Toutes trouvées de la même façon, et toutes du même genre : **un cas laisse
derrière lui un état que la suppression des index ne défait pas.**

Le premier passage élargi rendait 949/1173 contre ES 7.10.2, 46 échecs. Après
correction du runner : **992/1173, 3 échecs** — les 3 quirks du conteneur
ci-dessus. Les 43 autres n'étaient pas des défauts d'ES.

| Ce qui fuyait | Comment ça se présentait |
|---|---|
| un **template** posé vingt cas plus tôt | `mget` lisait un `_type` là où le cas attendait `null` ; `indices.stats` refusait d'indexer (« more than 1 type »). Rien qui ressemble à une fuite d'état : le template `t*` donnait un mapping `_doc` à tout index créé ensuite |
| un **dépôt** désenregistré qui garde ses fichiers | le cas suivant qui réenregistre le même chemin y retrouve les snapshots du précédent (« snapshot with the same name already exists ») |
| un **réglage de cluster** laissé en place | `cluster.put_settings` relisait le réglage du cas précédent |
| des **snapshots** dans le dépôt | supprimés avant lui, et dépôt par dépôt : le joker n'est pas accepté sur le dépôt |

Le nettoyage complet coûtait plus cher que la suite entière. Seules six API
posent cet état (`indices.put_template`, `indices.put_index_template`,
`cluster.put_component_template`, `ingest.put_pipeline`,
`snapshot.create_repository`, `cluster.put_settings`) : le balayage attend
qu'une d'elles ait été appelée. **3 min 26 pour 1173 cas**, contre 2 min 45
pour 643 avant.

## Le cliquet, en CI

Le job `conformance` de la CI compile ferrite, le lance, rejoue la suite et
compare la mesure fraîche au rapport commité :

```bash
python3 tests/compat/conformance_es.py http://127.0.0.1:9200 --etat \
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

### Le cliquet a battu — août 2026

**Ce qui s'est passé.** Pendant la carte 27, la CI de la PR #35 est passée rouge
puis verte **sans qu'une ligne ne bouge** : `indices.stats/14_groups.yml ::
Groups - star` tombait de `refus` à `echec`, 354 échecs devenaient 355, avec
pour raison `[index] 404 : no such index [test1]` — un échec dans la *mise en
place* du cas. Un cliquet qui bat ne vaut pas mieux qu'un dénominateur qu'on
écrit soi-même : il ouvre la porte au geste qui tuerait le reste du dossier,
relancer la CI jusqu'au vert.

**Reproduit d'abord.** Une campagne coûte 12 s contre un ferrite compilé en
`--release` : la boucle est donc le bon outil. Sur **cent campagnes
consécutives, six ont basculé** — et jamais sur le même cas (`14_groups.yml ::
Groups - star`, `11_metric.yml :: Metric - one`, `index/20_optype.yml ::
Optype`), toujours avec le même 404 dans la mise en place.

**L'hypothèse de départ était fausse.** La carte soupçonnait un alias survivant.
C'est ce qu'a servi à trancher le mode `--etat` (ci-dessous) : entre chaque
paire de cas, il vérifie qu'aucun index, alias, template, template d'index ni
réglage de cluster n'est apparu, et il est resté **vert pendant une campagne qui
a basculé**. `nettoie()` fait son travail ; la fuite n'était pas dans l'API.

**Le 404 était un masque.** `get_or_create` répondait `no such index` dès que la
création échouait, *quelle qu'en soit la raison* — un `Err(_) => self.get(name)`
écrit pour le cas « un autre appel a gagné la course ». Une fois l'erreur
réelle rendue, elle a nommé la cause en deux campagnes :

```
tantivy: Failed to acquire Lockfile: IoError(Os { code: 2, kind: NotFound })
tantivy: Failed to open file for read: FileDoesNotExist(".../test1/index-1/….term")
```

**La cause, écrite.** `refresh_dirty` travaille sur un instantané du catalogue :
entre le moment où elle prend la liste et celui où elle s'occupe d'un index, un
`DELETE` peut avoir retiré celui-ci. L'`Arc` qu'elle tient reste vivant — et ses
répertoires s'appellent `{index}/index-0`, `{index}/index-1`, **exactement ceux
qu'un index du même nom recréé juste après vient de s'attribuer**. Le vieux
balayage de générations efface alors la génération vivante du neuf, et le vieux
commit publie son `meta.json` par-dessus ses fichiers. Or `nettoie()` supprime
`test1` entre deux cas et le cas suivant le recrée aussitôt : la fenêtre est
grande ouverte, une fois sur dix-sept.

Ce n'était donc pas un défaut du runner mais **de ferrite**, et pas seulement
sous la suite de conformance : `DELETE /idx` puis réécriture immédiate est un
geste courant d'un script d'init. Deux corrections, dans
[`src/engine.rs`](../src/engine.rs) :

- un index retiré du catalogue est **marqué supprimé**, sous le verrou de
  rafraîchissement — le rafraîchissement de fond et le balayage de générations
  deviennent inertes sur lui ;
- la suppression **libère le nom par un renommage** (atomique) sous
  `.corbeille/` avant d'effacer. Plus aucun chemin n'est partagé entre un index
  et son homonyme, et `remove_dir_all` ne tombe plus sur le « Directory not
  empty » que tantivy provoque en poursuivant ses fusions après un commit. Ce
  qui n'a pas pu être effacé l'est à l'ouverture suivante du catalogue.

La course est figée hors d'une graine par
`tests/concurrence.rs::un_index_supprime_ne_touche_plus_aux_fichiers_de_son_homonyme`,
qui la joue **sans thread** : il suffit de garder l'`Arc`, comme le fait la
boucle de fond, et le test échoue sans chacune des deux corrections.

Elle ne l'est **pas** dans le harnais, et c'est mesuré plutôt que supposé : une
boucle `DELETE` / recréation posée par le client officiel contre le binaire
d'avant le correctif passe **400 tours sur 400**. La fenêtre est de quelques
microsecondes par seconde — un cas de conformance sur vingt mille — donc un
scénario qui la viserait par le haut serait vert quoi qu'il arrive. Un test qui
ne peut pas échouer ne mesure rien : c'est au test unitaire, qui supprime le
temps, de porter celui-là.

**Le déterminisme, vérifié.** **79 campagnes consécutives** — 40 sans le mode
`--etat`, 39 avec — rendent le même rapport **à l'octet près**, hors date et
SHA. C'était la propriété que la carte 01 revendiquait ; elle n'était donc pas
vraie. La mesure, elle, n'a pas bougé : 354 échecs, les mêmes cas, les mêmes
raisons — `docs/conformance.json` est inchangé.

### Vérifier l'état entre deux cas plutôt que le supposer

`--etat` relève, **entre chaque paire de cas**, huit sortes d'état — index,
alias, templates, templates d'index, templates de composants, réglages de
cluster, pipelines, dépôts de snapshots — et arrête la campagne au premier écart
plutôt que de laisser le cas suivant en hériter :

```
== etat verifie entre deux cas : index, alias, template, template d'index, reglage de cluster
   non verifiable (la cible ne sert pas la route) : template de composants, pipeline, depot de snapshots
```

Les sondes que la cible ne sait pas servir sont **imprimées**, pas passées sous
silence : un mode qui dirait « état propre » sans avoir posé la question serait
exactement le défaut qu'il corrige — le même que la sonde différentielle qui ne
trouvait qu'un serveur et annonçait « tout identique ».

**La référence n'est pas le vide, et c'est l'étalonnage qui l'a dit.** Écrit
contre l'idée qu'on se fait d'un serveur propre, le mode comparait à « rien ».
Lancé contre le conteneur de référence — le geste 2, avant de conclure quoi que
ce soit — il a crié dès le premier cas : un vrai Elasticsearch démarre avec ses
propres templates (`ilm-history`, `.transform-notifications-*`, les templates de
composants de x-pack) et il les **réinstalle** après que `nettoie()` les a
supprimés. Contre le vide, la seule cible qui sert à étalonner l'instrument
serait rouge de bout en bout.

La référence est donc **l'état de départ de la cible**, relevé avant le premier
nettoyage, et seules les **apparitions** par rapport à lui comptent. Une
disparition ne se lit pas : la réinstallation asynchrone des templates de x-pack
la rendrait aléatoire dans les deux sens.

Une fois retourné, le mode passe la suite entière contre le conteneur de
référence sans jamais crier, et la mesure y est celle du rapport commité —
**992/1173, 3 échecs**, à l'identique de
[`conformance-es7102.json`](conformance-es7102.json). Contre un vrai
Elasticsearch les **huit** sondes répondent, donc la vérification y est
complète ; contre ferrite, trois routes manquent (templates de composants,
pipelines, dépôts) et le mode le dit plutôt que de compter huit vérifications
là où il en fait cinq.

Coût mesuré : **3,3 s sur 12 s** par campagne, soit +27 %. C'est la CI qui le
paye, à chaque passage du cliquet — le job `conformance` passe `--etat`. En
développement, il reste optionnel.

Ce mode n'a pas trouvé la fuite d'août 2026 ; il a servi à **l'éliminer**, et
c'est autant son rôle. Il attrape la famille que la carte 02 avait payée trois
fois (template, dépôt, réglage de cluster survivants) — et celle-là ne se voit
qu'entre deux cas.

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

### Ce que l'élargissement a montré

Le dénominateur passe de 643 à 1173 cas. **530 cas apparaissent, et aucun cas
déjà mesuré ne bouge** — les 22 domaines d'avant rendent exactement le même
verdict qu'avant, ce qui est la preuve que l'élargissement n'a rien déplacé de
ce qui était déjà mesuré. Les deux taux baissent, et c'est le but : ils portaient
un dénominateur qu'on avait choisi.

Sur le territoire que ferrite déclare tenir, l'élargissement a trouvé **un vrai
manque, corrigé ici** : `indices.get_alias` échouait sur 10 de ses 31 cas —
`_all` rendait 404, les exclusions n'étaient pas lues, et le 404 partiel rendait
un corps vide. 9 des 10 sont corrigés (le dixième porte sur un index fermé, hors
périmètre), et la règle mesurée est dans
[`compat.md`](compat.md#lexpression-de-noms-dalias-sur-get-_aliasnom).

Le reste de ce territoire tient : `indices.put_alias` 11/12, `indices.exists_alias`
2/2, `indices.delete_alias` 11 refus explicites (tous sur le `routing` d'un
alias, un ❌ assumé), `scroll` 11 refus explicites (`rest_total_hits_as_int`,
`number_of_routing_shards`) et 1 échec (`search.default_keep_alive`, un réglage
de cluster non reconnu).

Deux manques francs sont **inscrits, pas corrigés** ici — ils demandent une
route neuve, pas un correctif : `GET /_cat/aliases` (10 cas ; ses tests exigent
aussi `h=`, `s=`, `help` sur les `_cat`) et `GET /{index}/_mapping/field/{champs}`
(15 cas). Voir [`compat.md`](compat.md).

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
| `unknown query [intervals]`, `unrecognized parameter: [version]`… | des refus **réels**, mais dont le type d'erreur imite celui d'Elasticsearch au lieu de porter le marqueur `not_implemented_in_ferrite_exception`. Le croisement avec `compat.yaml` les range en coût de périmètre : c'est ce que le motif d'erreur déclaré sur la capacité sert à dire |
| `include_type_name`, `_type` attendu dans la réponse | la suite est celle de la 7.10 ; ces cas testent ce que la **8.x a supprimé**. Un vrai ES 8 y échoue aussi |
| `indices.get` bloqués sur `_close` dans leur **mise en place** | l'échec n'est pas sur ce qu'ils mesurent |
| `_close` / `_open` | hors périmètre déclaré |
| `indices.delete` sur un motif ou sur un alias | ferrite suit la 8.x : `action.destructive_requires_name` vaut `true` et `DELETE /{alias}` est refusé. La suite est celle de la **7.10**, où le réglage valait `false` — un vrai ES 8 échoue au même endroit |
| `collapse`, `docvalue_fields`, `stored_fields`, agrégations non supportées… | hors périmètre déclaré, voir [`compat.md`](compat.md) |

## Ce que le runner ne fait pas

- Les fichiers `*_with_types.yml` sont ignorés : ils testent l'API typée
  (`/{index}/{type}/{id}`), supprimée en 8.x. Leur compte — fichiers **et** cas
  — est dans `mesure.exclusions`.
- Les cas qui exigent un verbe ou une `feature` que ce runner n'implémente pas
  sont comptés en `sautes`, et leur nombre est isolé dans `mesure.exclusions`.
- Les `skip: {version}` sont évalués **comme pour un serveur 7.10.2**, puisque
  c'est la version de la suite.
- `--suites` ne joue qu'une partie des domaines : le rapport le dit
  (`mesure.partiel`), et `--diff` refuse alors de trancher — une mesure
  partielle ne se compare pas à la suite entière.

Le rapport porte le nombre exact de cas joués, et ce qu'il laisse dehors avec
son compte : une exclusion sans son compte n'est pas vérifiable.
