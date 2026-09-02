# Les points de panique — l'inventaire

> Un `panic!` dans ferrite n'est pas une erreur 500. Le profil de release porte
> `panic = "abort"` ([`Cargo.toml`](../Cargo.toml)) : le processus entier meurt,
> et **tous les index qu'il servait deviennent injoignables**. Une panique
> atteignable depuis une requete n'est donc pas un defaut de robustesse parmi
> d'autres, c'est une panne generale declenchable par un client.

Ce fichier est le premier livrable de la carte 42. La question qu'il repond
n'est pas « le defaut signale est-il corrige ? » mais **« combien y en a-t-il
d'autres ? »** — et elle se repond par un relevé, pas par une impression.

Trois choses en sont sorties, et deux n'etaient pas dans la carte :

* le defaut signale (`copy_to` vers le sous-chemin d'une feuille) avait
  **onze** portes d'entree, dont une qui ne demande aucun `copy_to` :
  `{"a": {"b": "x"}}` sur un mapping ou `a` est un `keyword` ;
* une seconde famille, sans rapport, tuait le serveur sur **sept** routes :
  une borne de date dont le decalage se decoupe en octets (`+aéb`) ;
* une troisieme, trouvee en relisant ce relevé et par rien d'autre :
  `_validate/query?explain=true` avec une valeur qui **ressemble** a un numero
  de champ (`field=999999`) faisait indexer un tableau hors de ses bornes,
  dans tantivy.

---

## Le relevé

87 sites, tous les fichiers de `src/` et `src/api/` compris, hors `#[cfg(test)]`.
La commande qui les liste :

```bash
grep -rnE '\bpanic!\(|\bunreachable!\(|\btodo!\(|\bunimplemented!\(|\.expect\(|\.unwrap\(\)|\bassert(_eq|_ne)?!\(' src/
```

Chaque site est **soit** prouve inatteignable depuis une requete (avec la raison
ecrite au-dessus, dans le code), **soit** transforme en erreur. Le tableau
ci-dessous est le rangement ; la raison detaillee vit a cote du code.

### 1. Les verrous empoisonnes — 53 sites

`self.current.read().expect("generation lock")` et ses 52 jumeaux. Un
`RwLock`/`Mutex` de la bibliotheque standard rend `Err` quand un **autre** fil a
paniqué en le tenant.

**Inatteignables, et pour une raison qui tient a la compilation** : sous
`panic = "abort"` il n'y a pas de fil survivant a un `panic!`, donc pas
d'empoisonnement possible — le processus est deja mort. Sous `panic = "unwind"`
(ce que compile `cargo test`), ils ne se declenchent qu'apres une **premiere**
panique, qui serait le vrai defaut. Ils ne peuvent donc jamais etre la cause
d'un incident, seulement sa consequence.

### 2. Les invariants verifies deux lignes plus haut — 14 sites

| Site | Ce qui le garde |
|---|---|
| `aggs.rs:575` | `obj.len() != 1` refuse juste au-dessus |
| `api/aliases.rs:66` | `o.len() != 1` refuse juste au-dessus |
| `api/docs.rs:675` | `obj.len() != 1` refuse juste au-dessus |
| `dsl.rs:549`, `highlight.rs:439` | branche `1 =>` d'un `match` sur `len()` |
| `dsl.rs:1090` | branche `_ =>` d'un `match` sur `len()`, donc ≥ 2 |
| `dsl.rs:2333` | `valeur.as_object()` a deja rendu `Some` |
| `dsl.rs:2447` | `nom` vient de la liste que `Decroissance::lit` accepte |
| `dsl.rs:2469` | `champ.is_some()` est la garde du bras |
| `dsl.rs:2824` | `o.len() == 1` teste juste au-dessus |
| `dsl.rs:2857` | `s.contains_key("_name")` est la garde du bras |
| `highlight.rs:291-292` | le `match` au-dessus a rejete toutes les combinaisons ou l'un des deux est absent |
| `highlight.rs:1944` | `groupes.push(...)` a eu lieu si `ouvrir`, et `groupes` n'est jamais vide sinon |

### 3. Les listes non vides par construction — 6 sites

`segments.rs:120, 307, 511, 520, 522` (`*out.last()` / `*bornes.last()`) : les
deux vecteurs sont initialises a `vec![0usize]` et rien ne les vide.
`api/cluster.rs:148` (`&segments[..segments.len() - 1]`) : `split('.')` rend
toujours au moins un element.

### 4. Les variantes qu'aucun chemin ne construit — 4 sites

`aggs.rs:1453` et `fetch.rs:861` (`unreachable!`) : le bras precedent vient
d'inserer un `Value::Object` / un `Value::Array` a cette entree.
`langue.rs:184` : les langues de `rust-stemmers` que ferrite ne sert pas ne sont
construites nulle part.
`langue.rs:370` : `algorithme()` rend `Some` pour toute config qui a construit
un `SnowballStream`.

### 5. Les serialisations JSON — 7 sites

`alias.rs:184`, `templates.rs:519`, `engine.rs:886, 1857, 1907`,
`api/mod.rs:425, 446`. `serde_json::to_string` d'une `Value` ne peut echouer que
sur un `f64` non fini ou une cle non-chaine, et une `Value` **issue d'un
parse** n'en porte aucun : `serde_json` refuse `NaN` et `Infinity` a la lecture,
et les cles d'un objet JSON sont des chaines par definition.

### 6. Le reste — 3 sites

`dateformat.rs:65` : `DEFAUT` est une constante du fichier, et un test la lit.
`engine.rs:2093` : un `Index::create_in_ram` dont le reader ne peut pas echouer.
`explain.rs:426` : le numero de champ vient du `Debug` que **tantivy** pose en
contexte, pas d'une entree client — a la difference de son jumeau de
`api/validate.rs`, qui lisait le meme motif dans une chaine ou la **valeur
cherchee** figure aussi. C'est cette difference-la qui a fait le defaut.

### 7. Ceux qui n'etaient pas des invariants — corriges

| Site | Ce qui l'atteignait | Ce qu'il devient |
|---|---|---|
| `mapping.rs:818` (`expect("un prefixe de chemin est toujours un objet")`) | onze formes, voir ci-dessous | trois controles en amont, et la fonction ne panique plus (elle repose la feuille sous son nom pointe) |
| `dateformat.rs:417` (`&brut[..2]`) | `{"d": "2020-01-01T00:00:00+aéb"}`, sur sept routes | un decalage non-ASCII est refuse avant tout decoupage |
| `api/validate.rs:213` (`get_field_entry`) | `{"term": {"k": "field=999999"}}` sur `_validate/query?explain=true` | le numero est borne par le schema, et n'est plus lu **dans une valeur citee** |

---

## Ce qu'un grep ne trouve pas

Deux des trois familles corrigees paniquaient **dans une dependance**, pas dans
`src/` :

```
panicked at .../tantivy/src/schema/schema.rs:202:13:
Field already exists in schema a.b

panicked at .../tantivy/src/schema/schema.rs:...:
index out of bounds: the len is 5 but the index is 999999
```

Aucune relecture de `src/` ne les aurait listees : le `panic!` n'y est pas.
C'est la raison d'etre du troisieme livrable de la carte — un predicat de
**survie**, qui ne lit pas le code mais demande au serveur s'il est encore la.

Il y en a deux, et ils se completent :

* [`tests/compat/sonde_survie.py`](../tests/compat/sonde_survie.py) — 47 cas
  ecrits, chacun pose aux deux serveurs, avec `GET /` apres chacun. Contre le
  binaire corrige : **47/47 identiques a ES 8.15, 0 mort**. Contre le binaire
  0.10.0 : **30 cas MORT** ;
* [`fuzz_vs_es.py`](../tests/compat/fuzz_vs_es.py) — le predicat `survivant()`
  y est pose **apres chaque cas**, son verdict (`mort`) ne peut etre absorbe par
  aucune divergence assumee, et il nomme la **premiere requete restee sans
  reponse** au lieu de la derniere (qui serait le nettoyage). Deux briques de
  generateur l'alimentent : `doc.forme` (un objet pose sur une feuille) et
  `date.decalage_illisible`. Sur les graines 4200001–4200040, le binaire d'avant
  meurt des la premiere ; le binaire corrige rend 0 divergence.

## Ce que ce relevé ne prouve pas

Il ne prouve pas qu'il n'en reste aucun. Un relevé de `panic!` explicites ne
voit ni l'indexation d'une tranche, ni un depassement d'entier, ni le code des
dependances — et deux des trois defauts corriges etaient dans ces angles morts.
Ce qui reste apres lui, ce sont les deux predicats de survie ci-dessus.

## La piste non prise, et son prix

`panic = "abort"` est ce qui transforme une panique en panne **generale**. La
piste evidente est de passer le profil de release en `panic = "unwind"`. Elle
n'a pas ete prise dans cette carte, et ce n'est pas par prudence de principe :
elle est chiffree des deux cotes, et les deux chiffres sont mesures, pas
estimes.

**Ce qu'elle achete.** Le binaire d'avant la correction, recompile en `unwind`,
pose devant le document qui le tuait :

| | `abort` (aujourd'hui) | `unwind` (mesure) |
|---|---|---|
| la requete fautive | pas de reponse | `500` avec la phrase du `panic` |
| `GET /` juste apres | mort | `200` |
| ecriture sur un **autre** index | mort | `201` |
| recherche sur un **autre** index | mort | `200` |
| l'index fautif | mort | inutilisable (`PoisonError` sur son verrou de generation) |

Le rayon de l'incident passe donc du serveur entier a **un index**. C'est un
gain reel, et il en dit long sur les 53 `expect("… lock")` du relevé ci-dessus :
sous `unwind` ils cessent d'etre inatteignables, et deviennent la seconde moitie
du travail — un verrou empoisonne se rattrape (`PoisonError::into_inner`), il ne
se subit pas.

**Ce qu'elle coute.** Mesure sur le meme arbre, meme machine, `gzip -9` :

| | brut | compresse |
|---|---|---|
| `abort` | 10 108 280 | 4 064 422 |
| `unwind` | 11 358 568 | 4 687 443 |
| | +12,4 % | **+15,3 %** |

Le binaire compresse *est* pratiquement l'image publiee (voir
[`container.json`](container.json)) : le premier argument du projet passerait de
4,0 a ~4,6 Mo. Ce n'est pas un detail qu'on glisse dans un correctif urgent qui
part en release le jour meme — c'est un arbitrage produit, avec un cliquet de CI
a relancer (`measure_container.sh`, `chiffres_conteneur.py --injecte`) et une
liste de verrous a rattraper. Il a sa carte, pas sa ligne dans celle-ci.

## Trouve en passant, pas corrige

Le balayage a sorti des divergences qui ne sont pas des paniques et qui
n'entrent pas dans cette carte. Elles sont notees ici pour ne pas etre
re-trouvees trois fois :

| Question | ferrite | ES 8.15 |
|---|---|---|
| `PUT /{i}/_settings {"refresh_interval": "aéb"}` | 200 | 400 |
| `PUT /_cluster/settings` avec une valeur illisible | 200 | 400 |
| `aggs` dont le nom est vide, ou contient `>` | 200 | 400 |
| `terms` avec `size: 0` ou `size: -1` | 200 | 400 |
| `PUT /_index_template` dont le motif contient un accent | 200 | 400 |
| `_bulk` sans saut de ligne final, ou fait de lignes vides | 200 | 400 |
