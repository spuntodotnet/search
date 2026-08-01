# `nested` et `join` : par où on les prendrait

> Note de conception. Rien de ce qui suit n'est implémenté — mais rien n'y est
> supposé non plus : les deux propriétés de tantivy dont ces chemins dépendent
> sont vérifiées par `tests/spike_nested.rs`, qui tourne avec `cargo test`.

## Non, il n'y a pas Lucene à réécrire

C'est la première réaction, et elle est fausse : **tantivy *est* l'équivalent de
Lucene** — index inversé, postings compressés, FST, doc values colonnaires,
BM25. Le moteur est là, et `diff_relevance.py` mesure qu'il rend les mêmes
documents dans le même ordre qu'Elasticsearch sur 138 requêtes.

Ce qui manque n'est pas un moteur, c'est **une construction** : la jointure de
bloc. Chez Lucene elle repose sur deux choses :

| Ce dont Lucene a besoin | tantivy 0.26 |
|---|---|
| `IndexWriter.addDocuments()` — un groupe de documents écrit d'un seul tenant, docids contigus | **présent, non documenté comme tel** : `IndexWriter::run()` empile le lot dans un seul `AddBatch`, qu'un worker consomme entier dans *son* segment |
| `ToParentBlockJoinQuery` — remonter d'un enfant à son parent | **absent** — mais une `Query` sur mesure est déjà écrite ici (`src/dismax.rs`) |

Autrement dit : le sujet n'est pas un an de moteur, c'est une brique de requête,
plus des décisions de modèle. Ce qui coûte cher, ce n'est pas l'algorithme,
c'est ce que `nested` fait à toutes les autres réponses (les totaux, les
comptages, la suppression, les agrégations).

## Ce que les spikes ont mesuré

```
$ cargo test --test spike_nested -- --nocapture
qte  : [30, 10, 20]
ref  : ["zebre", "abeille", "zebre"]  (ords [1, 0, 1])
test ordre_des_valeurs_multivaluees ... ok
4 segments, 40 lots
test contiguite_des_documents_d_un_meme_run ... ok
```

1. **L'ordre des valeurs d'un champ multivalué est conservé** — ni trié, ni
   dédupliqué, pour les nombres comme pour les chaînes (`values_for_doc`,
   `term_ords`). Donc la *i*-ème valeur de `lignes.ref` et la *i*-ème de
   `lignes.qte` peuvent décrire le même sous-objet.
2. **Les documents d'un même `run()` forment un bloc** : même segment, docids
   consécutifs, ordre d'insertion respecté — vérifié avec 4 threads
   d'indexation et 40 lots entrelacés, qui se sont répartis sur 4 segments sans
   qu'aucun lot ne soit coupé.

La première ouvre un chemin **sans** jointure de bloc. La seconde ouvre le
chemin de Lucene. Elles ne mènent pas au même produit.

## `nested`, chemin A — des colonnes corrélées (recommandé)

L'idée : ne pas éclater le document. Un `nested` reste **un** document tantivy ;
chaque sous-champ devient une colonne multivaluée, et la corrélation entre
sous-champs se retrouve à la lecture.

Pour `lignes: [{ref: "A", qte: 5}, {ref: "B", qte: 20}]` :

```
lignes.ref       ["A", "B"]
lignes.ref@elem  [ 0,   1 ]
lignes.qte       [ 5,   20]
lignes.qte@elem  [ 0,   1 ]
```

La colonne `@elem` — l'indice du sous-objet auquel chaque valeur appartient —
est ce qui rend le procédé **général** : sans elle, un sous-objet auquel il
manque un champ décalerait toutes les positions suivantes. Avec elle, un champ
absent, un tableau imbriqué dans un sous-objet ou des sous-objets hétérogènes ne
posent plus de problème.

Une requête `nested` s'évalue alors en deux temps :

1. **Pré-filtre** : la conjonction des sous-clauses *sans* corrélation, en
   postings ordinaires. C'est un sur-ensemble exact des candidats, et c'est
   gratuit — c'est déjà ce que ferrite sait faire.
2. **Vérification** : pour chaque candidat, chaque sous-clause donne l'ensemble
   des `@elem` où elle est vraie ; leur intersection non vide, c'est un
   document qui correspond. Aucun faux positif, aucun faux négatif.

| | |
|---|---|
| **Ce que ça donne** | `nested` en **filtre**, exact. `inner_hits` est accessible : l'intersection donne l'indice des sous-objets qui correspondent, et `_source` (stocké entier) donne leur contenu |
| **Ce que ça préserve** | un document reste un document — les totaux, `_count`, la suppression, les agrégations, le tri, `_source` : rien ne bouge |
| **Ce que ça ne donne pas** | le scoring *à l'intérieur* du `nested` (`score_mode: avg/max/sum` sur du texte analysé) : les colonnes stockent la valeur, pas les postings. Un `nested` scoré devrait être refusé explicitement plutôt que rendu faux |
| **Ce que ça coûte** | à l'indexation, une colonne `@elem` par sous-champ. À la requête, la vérification est en O(candidats × valeurs) — c'est un filtre, pas une intersection de postings |

C'est le chemin qui ressemble à ferrite : mono-nœud, exact, et qui refuse ce
qu'il ne sait pas faire au lieu de l'approximer.

## `nested`, chemin B — le bloc de Lucene

Parent et enfants écrits dans un même `run()`, une colonne `_is_parent`, et une
`Query` sur mesure qui remonte de l'enfant au parent (le spike 2 montre que le
bloc tient).

C'est la sémantique exacte d'Elasticsearch, scoring compris. Mais le modèle
« un document = un document » saute, et il saute **partout** : `hits.total` doit
exclure les enfants, `_count` aussi, les agrégations aussi, la suppression d'un
parent doit emporter son bloc, une mise à jour doit le réécrire en entier, et
toute la comptabilité `_id → _seq_no` de `engine.rs` doit distinguer deux
espèces de documents. S'y ajoutent deux dépendances qu'il faudrait verrouiller :
la contiguïté n'est pas un contrat de tantivy (le spike la mesure, une montée de
version pourrait la reprendre), et la fusion de segments doit préserver l'ordre
relatif des documents.

À réserver au jour où le scoring intra-`nested` est vraiment demandé.

## `join` — plus simple qu'il n'y paraît, parce que ferrite est mono-nœud

Parent et enfant sont des documents distincts, réunis à la requête.
`has_child` / `has_parent` s'implémentent en deux passes :

1. exécuter la sous-requête (sur les enfants) ;
2. lire la colonne `_parent` des documents qui correspondent, matérialiser
   l'ensemble des identifiants, et le transformer en une recherche sur `_id`.

Exact, borné par le nombre de parents distincts. Elasticsearch ne peut pas se
le permettre — distribué, il lui faut des *global ordinals* et un cache dédié —
mais **mono-shard, la contrainte disparaît** : parents et enfants sont
forcément au même endroit. `score_mode` (`max`, `sum`, `avg`, `none`) s'agrège
dans la même passe.

Le vrai coût de `join` n'est pas la requête, c'est le champ `join` lui-même :
un type de champ à part, des relations déclarées dans le mapping, un `routing`
obligatoire à l'indexation des enfants, et l'`_id` du parent à valider.

## Les ordres de grandeur

| Chantier | Ce qu'il touche | Taille |
|---|---|---|
| `object` (sous-objets plats) | mapping récursif, parcours du document, re-nichage dans `_mapping` | **petit** — l'indexation est déjà par chemin pointé |
| `nested` chemin A | colonnes `@elem`, une clause DSL, `inner_hits` | **moyen** — rien d'autre ne bouge |
| `join` | un type de champ, `routing`, deux clauses DSL en deux passes | **moyen** |
| `nested` chemin B | tout ce qui compte des documents, dans tout le moteur | **gros**, et dépendant d'un détail non contractuel de tantivy |

L'ordre de mise en œuvre qui découle de ce tableau : `object`, puis `nested` A,
puis `join` — et `nested` B seulement si le scoring intra-`nested` devient un
besoin réel plutôt qu'une case à cocher.
