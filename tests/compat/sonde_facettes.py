#!/usr/bin/env python3
"""Sonde : ce qui separe une agregation `terms` d'une vraie facette.

`include` / `exclude`, l'ordre par **sous-agregation**, et leur cohabitation
avec `missing`, `size`, `shard_size` et les deux compteurs que la reponse
transporte (`sum_other_doc_count`, `doc_count_error_upper_bound`).

Elle compare le **bloc `terms` entier** : les seaux dans leur ordre, leurs
cles, leurs comptes, la valeur de chaque sous-agregation, et les deux
compteurs. Comparer le seul ensemble des cles ne mesurerait rien ici : un ordre
par sous-agregation faux garde les memes seaux, et les compteurs se calculent
**apres** filtrage — un `sum_other_doc_count` qui ignore l'`exclude` reste
plausible.

Deux corpus, parce que les regles ne se voient pas a la meme taille :

- `sonde-facettes` : huit categories, dont des majuscules et un chiffre, plus
  des documents **sans** categorie. C'est lui qui exerce la syntaxe de Lucene,
  le departage des ex aequo et les refus ;
- `sonde-facettes-gros` : 800 categories sur 6 000 documents, dont sept sur
  huit n'ont **aucun** prix. Il faut cette taille pour deux raisons : les
  seaux rares d'une sous-agregation ne se comptent juste qu'au-dela de 2 048
  documents par segment (voir `sonde_sous_aggs.py`), et
  `doc_count_error_upper_bound` ne bascule a `-1` que quand le nombre de
  termes distincts depasse ce que le shard collecte.

    python3 tests/compat/sonde_facettes.py [ferrite] [es]
    python3 tests/compat/sonde_facettes.py --calibrer [es_a] [es_b]

Elle **refuse de tourner** si elle ne trouve pas ses deux cibles : une sonde
differentielle qui ne compare rien rendrait « tout identique ».
"""
import json
import sys
import urllib.error
import urllib.request

PETIT = "sonde-facettes"
GROS = "sonde-facettes-gros"


def http(base, method, path, body=None, ndjson=False):
    data = None
    headers = {}
    if body is not None:
        if ndjson:
            data = body.encode()
            headers["Content-Type"] = "application/x-ndjson"
        else:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
    req = urllib.request.Request(base + path, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        corps = e.read()
        try:
            return e.code, json.loads(corps or b"{}")
        except json.JSONDecodeError:
            return e.code, {"raw": corps.decode(errors="replace")}


# Huit categories choisies pour les pieges de la syntaxe de Lucene : deux
# variantes de casse du meme mot (`alpha` / `ALPHA`), un tiret, un souligne
# suivi d'un chiffre. Un document sur treize n'a **pas** de categorie : c'est
# lui qui fait exister le seau de `missing`.
CATS = ["alpha", "beta", "gamma", "delta", "Epsilon", "alpha-bis", "zeta_1",
        "ALPHA"]


def docs_petit():
    out = {}
    for i in range(60):
        doc = {"prix": round(10 + (i % 7) * 3.5, 2), "n": i % 5,
               "d": "2026-01-%02d" % (1 + i % 28), "b": i % 2 == 0}
        if i % 13:
            doc["cat"] = CATS[i % len(CATS)]
        out[str(i)] = doc
    return out


def docs_gros():
    """800 categories, 6 000 documents, et un prix pour une categorie sur sept.

    Les categories sans prix sont ce qui compte : c'est sur elles que se lit ou
    ES classe une metrique **vide**, et la reponse n'est pas la meme pour
    `min`, `max` et `avg`."""
    out = {}
    for i in range(6000):
        rang = i % 800
        doc = {"cat": "cat-%04d" % rang}
        if rang % 7 == 0:
            doc["prix"] = float(rang % 50) + 0.5
        out[str(i)] = doc
    return out


def bulk(base, index, docs):
    lignes = []
    for id_, doc in docs.items():
        lignes.append(json.dumps({"index": {"_index": index, "_id": id_}}))
        lignes.append(json.dumps(doc))
    http(base, "POST", "/_bulk?refresh=true", "\n".join(lignes) + "\n", ndjson=True)


def prepare(base):
    http(base, "DELETE", f"/{PETIT}")
    http(base, "PUT", f"/{PETIT}", {"mappings": {"properties": {
        "cat": {"type": "keyword"}, "prix": {"type": "double"},
        "n": {"type": "long"}, "d": {"type": "date"},
        "b": {"type": "boolean"}}}})
    bulk(base, PETIT, docs_petit())

    http(base, "DELETE", f"/{GROS}")
    http(base, "PUT", f"/{GROS}", {"mappings": {"properties": {
        "cat": {"type": "keyword"}, "prix": {"type": "double"}}}})
    docs = docs_gros()
    for debut in range(0, len(docs), 2000):
        lot = {str(i): docs[str(i)] for i in range(debut, min(debut + 2000, len(docs)))}
        bulk(base, GROS, lot)


AVG = {"pm": {"avg": {"field": "prix"}}}
STATS = {"s": {"stats": {"field": "prix"}}}


def cas_include_exclude():
    """`include` / `exclude` : les deux formes, et ce qu'elles font aux compteurs."""
    out = []

    def t(libelle, corps, index=PETIT):
        out.append((libelle, index, {"c": {"terms": corps}}))

    # L'expression reguliere est celle de Lucene, et elle est **ancree** sur le
    # terme entier : `include: "a"` ne rend rien sur un terme `alpha`, et
    # `^alpha$` non plus puisque `^` et `$` y sont des litteraux.
    for motif in ["a.*", "a", "alpha", "^alpha$", ".*a", "[a-e]lpha", "zeta_\\d",
                  "@", "", "A.*", ".*", "alpha(-bis)?", "(alpha|beta)",
                  "[^a-z].*", "a.?pha", "z.+"]:
        t(f"include regexp {motif!r}", {"field": "cat", "include": motif})
    for motif in ["a.*", "zeta_\\d", ".*"]:
        t(f"exclude regexp {motif!r}", {"field": "cat", "exclude": motif})
    # Les quatre operateurs que ferrite refuse dans une requete `regexp` — le
    # refus doit etre le meme ici, et il doit se voir.
    for motif in ["zeta_<1-9>", "a.*&.*bis", "~alpha", "#"]:
        t(f"include regexp {motif!r}", {"field": "cat", "include": motif})
    # Un motif que Lucene lui-meme refuse.
    t("include regexp invalide", {"field": "cat", "include": "a["})

    # La liste exacte. ES lit chaque element **comme du texte** : un nombre y
    # devient le terme de son ecriture decimale.
    for liste in [["alpha"], ["alpha", "beta"], ["nexiste-pas"], [],
                  ["ALPHA", "alpha"], [1, 2], ["zeta_1"], ["alpha", "alpha"]]:
        t(f"include liste {liste!r}", {"field": "cat", "include": liste})
    for liste in [["alpha"], ["alpha", "beta"], [], ["nexiste-pas"]]:
        t(f"exclude liste {liste!r}", {"field": "cat", "exclude": liste})
    t("include + exclude", {"field": "cat", "include": "a.*",
                            "exclude": ["alpha"]})
    t("include liste + exclude regexp",
      {"field": "cat", "include": ["alpha", "ALPHA", "beta"], "exclude": "A.*"})

    # Les compteurs, apres filtrage. `sum_other_doc_count` ne doit compter que
    # les documents des seaux **retenus par le filtre** que la troncature a
    # ensuite ecartes ; les documents des termes exclus n'y sont pas du tout.
    for size in [1, 2, 3, 8]:
        t(f"size={size} sans filtre", {"field": "cat", "size": size})
        t(f"size={size} + exclude", {"field": "cat", "size": size,
                                     "exclude": ["alpha"]})
        t(f"size={size} + include", {"field": "cat", "size": size,
                                     "include": "a.*"})
    # `doc_count_error_upper_bound` bascule sur le nombre de termes **restants**.
    for shard in [1, 2, 3, 8, 9, 20]:
        t(f"count asc size=1 shard_size={shard}",
          {"field": "cat", "size": 1, "shard_size": shard,
           "order": {"_count": "asc"}})
        t(f"count asc size=1 shard_size={shard} + exclude",
          {"field": "cat", "size": 1, "shard_size": shard,
           "order": {"_count": "asc"}, "exclude": ["alpha", "beta", "gamma"]})

    # Les champs qui ne sont pas des chaines. ES sert la liste exacte sur un
    # numerique, une date et un booleen ; il refuse l'expression reguliere.
    for champ, liste in [("n", [1, 3]), ("d", ["2026-01-01"]), ("b", [True]),
                         ("prix", [10.0])]:
        t(f"include liste sur [{champ}]", {"field": champ, "include": liste})
        t(f"exclude liste sur [{champ}]", {"field": champ, "exclude": liste})
        t(f"include regexp sur [{champ}]", {"field": champ, "include": "1.*"})

    # La forme partitionnee.
    for p in [0, 1, 2]:
        t(f"include partition {p}/3",
          {"field": "cat", "size": 100,
           "include": {"partition": p, "num_partitions": 3}})
    t("include objet invalide", {"field": "cat", "include": {"quoi": 1}})
    t("include liste d'objets", {"field": "cat", "include": [{"a": 1}]})

    # `missing` et un filtre sur la meme agregation.
    out.append(("missing + include", PETIT, {"c": {"terms": {
        "field": "cat", "missing": "(vide)",
        "include": ["alpha", "(vide)"]}}}))
    out.append(("missing + exclude", PETIT, {"c": {"terms": {
        "field": "cat", "missing": "(vide)", "exclude": ["alpha"]}}}))
    out.append(("missing seul", PETIT, {"c": {"terms": {
        "field": "cat", "missing": "(vide)"}}}))

    # Sur le gros corpus : un filtre qui garde plus de termes que la marge que
    # ferrite se donne.
    t("gros : include cat-0[0-1]..", {"field": "cat", "include": "cat-0[0-1]..",
                                      "size": 600}, GROS)
    t("gros : exclude cat-0[0-4].*, count asc",
      {"field": "cat", "exclude": "cat-0[0-4].*", "size": 3,
       "order": {"_count": "asc"}}, GROS)
    return out


def cas_ordre():
    """L'ordre par sous-agregation : le morceau qui demande de calculer les
    sous-agregations **avant** de trier les seaux."""
    out = []

    def t(libelle, corps, sous, index=PETIT):
        out.append((libelle, index, {"c": {"terms": corps, "aggs": sous}}))

    for metrique in ["avg", "min", "max", "sum", "value_count"]:
        for sens in ["asc", "desc"]:
            sous = {"pm": {metrique: {"field": "prix"}}}
            t(f"order {metrique} {sens}",
              {"field": "cat", "size": 3, "order": {"pm": sens}}, sous)
            # Le meme, sur le corpus ou la metrique est **vide** pour sept
            # categories sur huit : c'est la que `min`, `max` et `avg` cessent
            # de se ressembler.
            t(f"gros : order {metrique} {sens}",
              {"field": "cat", "size": 6, "order": {"pm": sens}}, sous, GROS)
    for prop in ["count", "min", "max", "avg", "sum"]:
        for sens in ["asc", "desc"]:
            t(f"order stats.{prop} {sens}",
              {"field": "cat", "size": 3, "order": {"s." + prop: sens}}, STATS)
            t(f"gros : order stats.{prop} {sens}",
              {"field": "cat", "size": 6, "order": {"s." + prop: sens}},
              STATS, GROS)

    # Le departage des ex aequo : trois categories de meme moyenne.
    t("order avg desc, ex aequo",
      {"field": "cat", "size": 5, "order": {"pm": "desc"}}, AVG)
    t("order avg asc, ex aequo",
      {"field": "cat", "size": 5, "order": {"pm": "asc"}}, AVG)
    # La casse du sens : ES la lit sans y regarder.
    for sens in ["DESC", "Asc"]:
        t(f"order avg {sens!r}",
          {"field": "cat", "size": 2, "order": {"pm": sens}}, AVG)
        out.append((f"order _key {sens!r}", PETIT,
                    {"c": {"terms": {"field": "cat", "size": 2,
                                     "order": {"_key": sens}}}}))

    # Les compteurs sous un ordre par sous-agregation.
    for size in [1, 5, 799, 800]:
        t(f"gros : order avg desc size={size}",
          {"field": "cat", "size": size, "order": {"pm": "desc"}}, AVG, GROS)
    for shard in [1, 8, 9]:
        t(f"order avg desc size=1 shard_size={shard}",
          {"field": "cat", "size": 1, "shard_size": shard,
           "order": {"pm": "desc"}}, AVG)
    for shard in [799, 800, 801]:
        t(f"gros : order avg desc size=5 shard_size={shard}",
          {"field": "cat", "size": 5, "shard_size": shard,
           "order": {"pm": "desc"}}, AVG, GROS)

    # Avec un filtre, avec `missing`, et avec une sous-agregation de seaux a
    # cote de la metrique qui sert de cle de tri.
    t("include + order avg desc",
      {"field": "cat", "size": 4, "include": "a.*", "order": {"pm": "desc"}},
      AVG)
    t("gros : include + order avg desc",
      {"field": "cat", "size": 4, "include": "cat-00.*", "order": {"pm": "desc"}},
      AVG, GROS)
    t("missing + order avg desc",
      {"field": "cat", "size": 4, "missing": "(vide)", "order": {"pm": "desc"}},
      AVG)
    t("order avg desc + sous-agg de seaux",
      {"field": "cat", "size": 3, "order": {"pm": "desc"}},
      {"pm": {"avg": {"field": "prix"}}, "n2": {"terms": {"field": "n", "size": 2}}})
    t("order avg desc sur champ date",
      {"field": "d", "size": 3, "order": {"pm": "desc"}}, AVG)
    t("order avg desc sur champ numerique",
      {"field": "n", "size": 3, "order": {"pm": "desc"}}, AVG)

    # Les chemins qu'ES refuse, un par raison.
    t("order : agregation inconnue",
      {"field": "cat", "order": {"zz": "desc"}}, AVG)
    t("order : agregation de seaux",
      {"field": "cat", "order": {"sub": "desc"}},
      {"sub": {"terms": {"field": "n"}}})
    # Une agregation **mono-seau** est une cle d'ordre valable chez ES — il
    # classe sur son `doc_count`, que le chemin le nomme ou non. C'est le cas
    # que porte la suite de conformance d'Elastic, et c'est un cout de
    # perimetre : `filter` est deja refusee sous une agregation de seaux.
    t("order : agregation mono-seau",
      {"field": "cat", "order": {"h": "desc"}},
      {"h": {"filter": {"range": {"prix": {"gte": 20}}}}})
    t("order : mono-seau .doc_count",
      {"field": "cat", "order": {"h.doc_count": "desc"}},
      {"h": {"filter": {"range": {"prix": {"gte": 20}}}}})
    t("order : stats sans propriete",
      {"field": "cat", "order": {"s": "desc"}}, STATS)
    t("order : stats propriete inconnue",
      {"field": "cat", "order": {"s.variance": "desc"}}, STATS)
    # `value` est le nom de la seule valeur d'une metrique simple : ES
    # l'accepte, et refuse tout autre nom.
    t("order pm.value desc", {"field": "cat", "size": 2,
                              "order": {"pm.value": "desc"}}, AVG)
    t("gros : order pm.value desc", {"field": "cat", "size": 4,
                                     "order": {"pm.value": "desc"}}, AVG, GROS)
    for metrique in ["min", "max"]:
        t(f"gros : order {metrique}.value desc",
          {"field": "cat", "size": 4, "order": {"pm.value": "desc"}},
          {"pm": {metrique: {"field": "prix"}}}, GROS)
    t("order : metrique simple, propriete inconnue",
      {"field": "cat", "order": {"pm.count": "desc"}}, AVG)
    t("order : chemin a deux niveaux",
      {"field": "cat", "order": {"f>pm": "desc"}}, AVG)
    t("order : sens invalide",
      {"field": "cat", "order": {"pm": "haut"}}, AVG)
    out.append(("order : sans sous-agregation", PETIT,
                {"c": {"terms": {"field": "cat", "order": {"pm": "desc"}}}}))
    return out


# Les ecarts qu'on assume, avec leur raison. Un cas n'y compte que si ferrite
# **refuse** : la liste ne peut donc pas absorber un cas ou ferrite repondrait
# autre chose qu'ES en 200.
REFUS_ASSUMES = {
    "include regexp 'zeta_<1-9>'":
        "l'intervalle numerique de Lucene, refuse partout dans ferrite : "
        "l'automate du crate `regex` ne sait pas le construire. Meme refus que "
        "dans une requete `regexp`",
    "include regexp 'a.*&.*bis'":
        "l'intersection de Lucene, meme raison et meme refus que dans une "
        "requete `regexp`",
    "include regexp '~alpha'":
        "le complement de Lucene, meme raison et meme refus",
    "include regexp '#'":
        "la chaine vide de Lucene, meme raison et meme refus",
    "include liste sur [n]":
        "l'agregation de tantivy ne filtre les termes que sur une colonne de "
        "chaines et **ecarte la colonne entiere** quand elle ne l'est pas : "
        "elle rendrait zero seau la ou ES en rend. Refuse plutot que rendu faux",
    "exclude liste sur [n]": "meme raison",
    "include liste sur [d]": "meme raison",
    "exclude liste sur [d]": "meme raison",
    "include liste sur [b]": "meme raison",
    "exclude liste sur [b]": "meme raison",
    "include liste sur [prix]": "meme raison",
    "exclude liste sur [prix]": "meme raison",
    "include partition 0/3":
        "la forme partitionnee retient un terme selon un hachage de sa valeur "
        "(murmur3 x86_32, graine 31 — mesure, et stable au redemarrage d'ES). "
        "Ni le filtre par expression reguliere de tantivy ni sa liste exacte ne "
        "savent l'exprimer sans enumerer tout le dictionnaire, ce qui defait la "
        "raison d'etre du parametre",
    "include partition 1/3": "meme raison",
    "include partition 2/3": "meme raison",
    "missing + include":
        "le seau de remplissage de `missing` n'a pas d'identifiant dans le "
        "dictionnaire de termes : le filtre de tantivy l'ecarte toujours, alors "
        "qu'ES le traite comme un terme ordinaire. Refuse plutot que perdu en "
        "silence",
    "missing + exclude": "meme raison",
    "order : chemin a deux niveaux":
        "ES descend a travers une agregation mono-seau ; la seule que ferrite "
        "serve (`filter`) est deja refusee sous une agregation de seaux",
    "order : agregation mono-seau":
        "ES classe alors les seaux sur le `doc_count` de l'agregation "
        "mono-seau ; ferrite refuse deja `filter` sous une agregation de "
        "seaux, donc le chemin ne mene nulle part. C'est le cas [Sorting "
        "terms] de la suite de conformance d'Elastic",
    "order : mono-seau .doc_count": "meme raison, chemin nomme",
}


def interroge(base, index, aggs):
    """Le bloc `terms` entier, ou le statut de l'erreur.

    Sur une erreur, seul le **statut** se compare : ES empile ses erreurs de
    recherche sous un `search_phase_execution_exception` dont la `root_cause`
    porte le vrai type, la ou ferrite rend l'erreur directement. C'est une
    divergence connue et documentee, pas un effet de ces parametres."""
    st, body = http(base, "POST", f"/{index}/_search?size=0",
                    {"aggs": aggs, "track_total_hits": True})
    if st != 200:
        err = body.get("error", {})
        if isinstance(err, dict):
            racine = err.get("root_cause") or [{}]
            ty = racine[0].get("type") or err.get("type", "?")
        else:
            ty = "?"
        return str(st), f"{st} {ty}"
    agg = body.get("aggregations", {}).get("c", {})
    # `json.dumps` avec `sort_keys` : l'ordre des cles d'un seau n'est pas ce
    # qu'on mesure, celui des seaux l'est — et il est porte par la liste.
    return json.dumps(agg, sort_keys=True), json.dumps(agg, sort_keys=True)


def abrege(vu, n=220):
    return vu if len(vu) <= n else vu[:n - 3] + "..."


def main():
    argv = [a for a in sys.argv[1:] if a != "--calibrer"]
    calibrer = "--calibrer" in sys.argv
    gauche = argv[0] if argv else ("http://localhost:9201" if calibrer
                                   else "http://localhost:9200")
    droite = argv[1] if len(argv) > 1 else ("http://localhost:9202" if calibrer
                                            else "http://localhost:9201")
    cibles = [("es_a" if calibrer else "ferrite", gauche),
              ("es_b" if calibrer else "es", droite)]
    for nom, base in cibles:
        try:
            http(base, "GET", "/")
        except Exception as e:  # noqa: BLE001
            print(f"# {nom} indisponible ({base}) : {e}")
            print("# une sonde differentielle qui ne compare rien ne rend pas "
                  "de verdict : arret.")
            return 2
    for _, base in cibles:
        prepare(base)

    batterie = cas_include_exclude() + cas_ordre()
    ecarts = assumes = total = 0
    for libelle, index, aggs in batterie:
        reps = [(nom, *interroge(base, index, aggs)) for nom, base in cibles]
        total += 1
        if len({cle for _, cle, _ in reps}) <= 1:
            print(f"  {libelle:44} {abrege(reps[0][2])}")
            continue
        # Un ecart n'est assume que s'il est ecrit **et** que c'est bien la
        # cible de gauche (ferrite, hors `--calibrer`) qui refuse : sans cette
        # seconde moitie, la liste couvrirait aussi un resultat different rendu
        # en 200.
        gauche_refuse = not reps[0][1].startswith("{")
        if libelle in REFUS_ASSUMES and gauche_refuse and not calibrer:
            assumes += 1
            marque = "~"
        else:
            ecarts += 1
            marque = "*"
        print(f"{marque} {libelle:44} " + "\n      ".join(
            f"{nom}={abrege(vu)}" for nom, _, vu in reps))
        if marque == "~":
            print(f"      assume : {REFUS_ASSUMES[libelle]}")
    print(f"\n{total - ecarts - assumes}/{total} identiques, "
          f"{assumes} refus assumes, {ecarts} ecarts")
    return 1 if ecarts else 0


if __name__ == "__main__":
    sys.exit(main())
