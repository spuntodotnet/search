# Pourquoi ferrite n'installe pas le tantivy de crates.io

`Cargo.toml` porte un `[patch.crates-io]` : tantivy vient d'un commit d'un fork,
pas de la version publiée. C'est le genre de chose qui, six mois plus tard, n'a
plus d'explication et ne s'enlève plus. Ce fichier est là pour que ça n'arrive
pas — il dit **ce que l'épingle contient**, **pourquoi elle existe**, **ce qui
la tient**, et **comment en sortir**.

```toml
[patch.crates-io]
tantivy = { git = "https://github.com/spuntodotnet/tantivy", rev = "a8ffd13238c279aa1b529d1b94fb58d1b840fecd" }
```

## Le défaut

tantivy 0.26.1 rend des **valeurs fausses en 200** dans les sous-agrégations des
buckets rares.

`LowCardSubAggCache::flush_local` (`src/aggregation/cached_sub_aggs.rs`) vide son
cache tous les 2 048 documents d'un segment. Sur un vidage non forcé, il ne
recopie que les buckets au-dessus d'un seuil — puis appelle `clear()`, qui efface
**tous** les buckets, y compris ceux qu'il vient de sauter. Les documents cachés
de ces buckets-là ne sont jamais comptés.

Les `doc_count` des buckets, eux, sont tenus ailleurs et restent exacts. La
réponse est donc un 200 bien formé, avec le bon nombre de documents affiché juste
à côté de la valeur fausse. C'est le pire résultat possible dans ce projet : pas
un refus, pas une erreur, un chiffre.

Trouvé par le banc à l'échelle ([`bench.md`](bench.md)) : sur deux millions de
documents de la track Rally `geonames`, un `range` dont le bucket
`100.0-*` compte **28 518 documents** rendait un `value_count` de **1 692** —
94 % de perdus.

## Ce que la mesure a ajouté à la lecture du code

Tout ce qui suit sort de `tests/compat/sonde_sous_aggs.py`, posé aux deux
serveurs (ferrite et un vrai Elasticsearch 8.15.0). Aucune de ces bornes n'a été
déduite du code de tantivy : la carte demandait explicitement qu'elles soient
reproduites, pas estimées.

| Question | Mesure |
|---|---|
| À partir de combien de documents par segment ? | **2 047 juste, 2 048 faux.** Le seuil est exact, pas approximatif — c'est `FLUSH_THRESHOLD` |
| À partir de combien de documents par bucket ? | un bucket est perdu s'il a **au plus `2048 / (2 × nombre de buckets)`** documents dans la fenêtre qui se vide : **204 perdus, 205 gardés** sur 5 buckets |
| Quelles sous-agrégations ? | **toutes** — `value_count`, `sum`, `avg`, `min`, `max`, `stats`, et aussi les sous-agrégations de **buckets** (`terms`, `range`, `histogram`). Pas seulement `value_count` |
| Quels parents ? | un `terms` de **premier niveau** sous 100 valeurs distinctes, et **tout** `range`. Un `terms` à 199 valeurs, un `histogram`, et le `filter` qu'exécute ferrite étaient corrects |
| Combien de formes fausses ? | **14 sur 46** combinaisons parent × sous-agrégation, sur 50 000 documents |

Deux de ces lignes changent la gravité du défaut par rapport à ce qui en était
publié. La première : ce n'était pas « `value_count` est faux », c'était
« toutes les valeurs sont fausses ». La seconde : `avg` rendait **21,5** là où
Elasticsearch rend **21,428571428571427** — un nombre faux **plausible**, celui
qu'un tableau de bord affiche sans que personne ne sourcille. `min` rendait
`20.0` au lieu de `0.0`, `max` `23.0` au lieu de `38.0`.

## Le correctif

Il n'est pas de nous. Le défaut a été signalé en amont
([tantivy#2992](https://github.com/quickwit-oss/tantivy/issues/2992)) et corrigé
par le mainteneur le 9 juillet 2026 (commit `61693134`, *fix cache flush in
aggregations*) : le seuil par bucket disparaît, `flush_local` recopie **tous**
les buckets avant d'effacer le cache. Trois lignes ajoutées, vingt-neuf
retirées.

**Il n'est pas publié.** 0.26.1 reste la dernière version sur crates.io ; la
branche principale de tantivy est à 0.27.0 de développement, **211 commits**
plus loin — dont le remplacement du moteur de stemming, une refonte des
agrégations et une montée de `base64`. Passer sur cette branche pour un
correctif de trente lignes reviendrait à adopter, sans mesure, 211 changements
dans la dépendance dont ce projet mesure la compatibilité ligne à ligne. C'est
le contraire de ce que fait ce dépôt.

D'où le fork : le **tag 0.26.1**, plus ce seul commit. Le correctif d'amont ne se
`cherry-pick` pas tel quel — le fichier a été renommé `buffered_sub_aggs.rs`
après 0.26.1 — mais le changement, lui, est identique au caractère près.

Et le chiffre par lequel tout a commencé a été **rejoué sur le vrai corpus**,
plutôt que déduit de la sonde : 2 000 000 de documents de la track `geonames`
indexés dans les deux serveurs, la même agrégation posée aux deux.

```
-- range(population) > value_count
   bucket *-100.0    doc_count=1971482   value_count ferrite=1971482   ES=1971482   ok
   bucket 100.0-*    doc_count=28518     value_count ferrite=28518     ES=28518     ok
   JSON identique : True
```

Le bucket de **28 518** documents rendait **1 692**. Il rend 28 518, et la
réponse entière est identique à celle d'Elasticsearch.

## Ce que l'épingle contient exactement

Épingler un dépôt git à la place de crates.io remplace **neuf** paquets d'un
coup : tantivy entraîne avec lui les crates de son espace de travail. « C'est
juste le correctif d'amont » est précisément le genre de phrase que ce dépôt
n'accepte pas sans mesure.

`tests/compat/verifie_tantivy.py` la remplace par une comparaison : il télécharge
les crates **publiées** sur crates.io, extrait l'arbre du fork au commit épinglé,
et compare fichier par fichier. Sortie constatée :

```
== https://github.com/spuntodotnet/tantivy
   commit a8ffd13238c279aa1b529d1b94fb58d1b840fecd
   9 paquets pris sur le fork au lieu de crates.io

  ownedbytes 0.9.0 : identique a la crate publiee
  tantivy 0.26.1 : 1 fichier(s) different(s) de la crate publiee
      src/aggregation/cached_sub_aggs.rs
  tantivy-bitpacker 0.10.0 : identique a la crate publiee
  tantivy-columnar 0.7.0 : identique a la crate publiee
  tantivy-common 0.11.0 : identique a la crate publiee
  tantivy-query-grammar 0.26.0 : identique a la crate publiee
  tantivy-sstable 0.7.0 : identique a la crate publiee
  tantivy-stacker 0.7.0 : identique a la crate publiee
  tantivy-tokenizer-api 0.7.0 : identique a la crate publiee
```

Les empreintes des archives comparées sont **exactement** les `checksum` que
`Cargo.lock` portait avant l'épingle (`edde6a10…` pour tantivy 0.26.1, et ainsi
de suite) : ce ne sont donc pas « des crates » qu'on compare, ce sont celles-là.

Le script échoue si un fichier de plus diffère, si un fichier publié manque du
fork, ou si l'écart déclaré disparaît. Il tourne dans la CI (job `tantivy`).

## Ce qui tient l'épingle

Trois choses, qui ne disent pas la même :

| | Ce qu'elle vérifie |
|---|---|
| `tests/spike_sous_aggs.rs` | le **comportement de la dépendance**, dans `cargo test`, donc à chaque PR. Trois cas : un `terms` à 90 valeurs déséquilibré, un `range`, et la borne des 2 048 documents. Ils échouent tous les trois sur un tantivy 0.26.1 non corrigé — vérifié |
| `tests/compat/verifie_tantivy.py` | le **contenu** de l'épingle : 0.26.1 à l'octet près, plus un fichier |
| `tests/compat/sonde_sous_aggs.py` | la **comparaison à un vrai Elasticsearch**, à l'échelle où le défaut vit. 46/46 avec l'épingle, **32/46 sans** — et 7/7 contre 3/7 sur les deux bornes en `--seuil` |

Le dernier chiffre est celui qui compte : une sonde qui ne rendrait pas
massivement rouge sur le ferrite d'avant ne mesurerait rien.

## Le coût

Non mesurable sur la machine de développement, et il faut le dire comme ça
plutôt que de publier un chiffre qu'on ne tient pas. Sur 500 000 documents, huit
agrégations, quatre tours alternés entre les deux binaires : les cas que le
correctif **ne touche pas** (`histogram > sum`, un `terms` sans
sous-agrégation) bougent de −11 % à +3 % d'un tour à l'autre, donc plus que ceux
qu'il touche. Le banc contrôlé du mainteneur, lui, donne +7,8 % à +11,6 % sur
les agrégations concernées, −7,7 % à +0,3 % ailleurs, et des kilo-octets de
mémoire en plus.

C'est de toute façon la mauvaise question : la version « rapide » ne comptait
pas tous les documents.

## Comment en sortir

Le jour où le correctif est publié sur crates.io — tantivy 0.27.0 ou une 0.26.2 :

1. remonter `tantivy` dans `[dependencies]` et `[dev-dependencies]` à cette
   version, **supprimer** le bloc `[patch.crates-io]` et le commentaire qui le
   précède ;
2. supprimer `tests/compat/verifie_tantivy.py` et le job CI `tantivy` — il n'a
   plus rien à vérifier ;
3. **garder** `tests/spike_sous_aggs.rs` : c'est lui qui dira que la nouvelle
   version tient vraiment la propriété, plutôt qu'on la suppose depuis un
   numéro de version. C'est le geste 7 de [`../CLAUDE.md`](../CLAUDE.md) ;
4. relancer `sonde_sous_aggs.py` et `sonde_sous_aggs.py --seuil` contre un vrai
   Elasticsearch, et mettre le chiffre à jour ici ;
5. supprimer la branche `ferrite/0.26.1-flush-sous-aggs` du fork, ou le fork.

Tant que ce n'est pas fait, une montée de version de tantivy passe **par** ce
fichier : `cargo update` ne peut pas décider seul de rendre des valeurs fausses.
