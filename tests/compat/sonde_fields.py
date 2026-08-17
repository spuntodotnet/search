#!/usr/bin/env python3
"""Sonde : que transporte *vraiment* une reponse d'Elasticsearch ?

`fields`, `docvalue_fields` et `stored_fields` sont les trois facons de
demander autre chose que le `_source` complet. Ce qui les separe n'est ecrit
nulle part : elles ne lisent pas au meme endroit, et ca se voit sur les bords.

    python3 tests/compat/sonde_fields.py [ferrite] [es]

Ce qui se compare ici, ce n'est pas un compte de resultats : c'est le **hit
entier**, `_score` mis a part — le bloc `fields` cle par cle, la presence de
`_source`, la presence de `_id`. C'est la forme qui compte : un client qui
connait `fields` sait que chaque valeur y est un **tableau**, meme pour un
champ mono-value ; un scalaire lui casserait le typage sans rien dire.

Les questions que cette sonde separe, et qu'aucune lecture ne tranchait :

- `fields` lit le `_source` : il garde l'ordre du document **et ses doublons**
  (`["b","a","b"]`) ;
- `docvalue_fields` lit les colonnes : un `keyword` en ressort trie **et
  dedoublonne** (`["a","b"]`), un numerique trie **avec** ses doublons
  (`[1,1,3]`), et un `float` avec la precision de son stockage 32 bits
  (`0.1` devient `0.10000000149011612`) ;
- `stored_fields` ne rend aucun champ tant qu'aucun n'est `store: true` — mais
  il retire `_source`, et `_none_` retire aussi `_id` ;
- un champ absent n'a **pas de cle** : ce n'est pas une valeur nulle ;
- un motif `*` ne ramene pas les metadonnees, un nom explicite si.
"""
import json
import sys
import urllib.error
import urllib.request

INDEX = "sonde-fields"
# Un second index, `dynamic: false` : son `_source` porte des chemins que le
# mapping ne connait pas. C'est le seul etat ou `include_unmapped` a quelque
# chose a rendre — sans lui, il n'y a rien de non mappe a trouver.
INDEX_LIBRE = "sonde-fields-libre"


def http(base, method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        base + path, data=data, method=method,
        headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        corps = e.read()
        try:
            return e.code, json.loads(corps or b"{}")
        except json.JSONDecodeError:
            return e.code, {"raw": corps.decode(errors="replace")}


MAPPING = {"mappings": {"properties": {
    "titre": {"type": "text", "fields": {"keyword": {"type": "keyword"}}},
    "tag": {"type": "keyword"},
    "n": {"type": "integer"},
    "l": {"type": "long"},
    "f": {"type": "float"},
    "dbl": {"type": "double"},
    "b": {"type": "boolean"},
    "d": {"type": "date"},
    "dfmt": {"type": "date", "format": "yyyy-MM-dd"},
    "client": {"properties": {"ville": {"type": "keyword"},
                              "cp": {"type": "integer"}}},
    "lignes": {"type": "nested", "properties": {
        "ref": {"type": "keyword"},
        "q": {"type": "integer"},
        # Un objet **dans** un `nested` : le chemin profond se rend avec sa
        # cle relative a la racine (`sous.z`). ferrite refuse un `nested` dans
        # un `nested`, c'est une capacite declaree refusee.
        "sous": {"properties": {"z": {"type": "keyword"}}}}},
    "jamais": {"type": "keyword"},
    # Une cle de tri stable des deux cotes : `_id` ne se trie pas chez ES
    # (« Fielddata access on the _id field is disallowed »).
    "ord": {"type": "keyword"},
}}}

DOCS = {
    # Le document complet : multivalue, doublons, ordre non trie, dates.
    "1": {
        "ord": "1",
        "titre": "le grand bleu", "tag": ["zoulou", "alpha", "alpha"],
        "n": [3, 1, 1], "l": 9007199254740993, "f": [0.1, 2.5, 0.1],
        "dbl": 0.1, "b": True, "d": ["2026-03-15", "2020-01-01T12:00:00Z"],
        "dfmt": "2026-03-15",
        "client": {"ville": "Lyon", "cp": 69000},
        "lignes": [{"ref": "X1", "q": 2},
                   {"q": 5},
                   {"ref": "X3", "sous": [{"z": "z1"}, {"z": "z2"}]}],
    },
    # Le document creux : la moitie des champs manquent. C'est lui qui montre
    # qu'un champ absent n'a pas de cle du tout.
    "2": {"ord": "2", "titre": "petit", "n": 7},
    # Les coercions : le `_source` ne porte pas le type du mapping.
    "3": {"ord": "3", "tag": 42, "n": "7", "b": "false", "d": 1600000000000,
          "lignes": {"ref": "seul"}},
    # Les `null`, qui ne se rendent pas.
    "4": {"ord": "4", "tag": [None, "x"], "n": None},
}


def bulk(base, docs):
    lignes = []
    for id_, doc in docs.items():
        lignes.append(json.dumps({"index": {"_index": INDEX, "_id": id_}}))
        lignes.append(json.dumps(doc))
    corps = "\n".join(lignes) + "\n"
    req = urllib.request.Request(
        base + "/_bulk?refresh=true", data=corps.encode(), method="POST",
        headers={"Content-Type": "application/x-ndjson"})
    urllib.request.urlopen(req).read()


def prepare(base):
    """Pose l'index et les documents — ou echoue bruyamment.

    Un mapping refuse laisserait le serveur inventer le sien, et la sonde
    mesurerait alors deux index differents : c'est exactement le defaut
    d'outillage que ce depot a paye plusieurs fois.
    """
    http(base, "DELETE", f"/{INDEX}")
    st, rep = http(base, "PUT", f"/{INDEX}", MAPPING)
    if st != 200:
        raise RuntimeError(f"mapping refuse ({st}) : {json.dumps(rep)[:300]}")
    bulk(base, DOCS)

    http(base, "DELETE", f"/{INDEX_LIBRE}")
    st, rep = http(base, "PUT", f"/{INDEX_LIBRE}", {"mappings": {
        "dynamic": False, "properties": {"a": {"type": "keyword"}}}})
    if st != 200:
        raise RuntimeError(f"mapping refuse ({st}) : {json.dumps(rep)[:300]}")
    corps = (json.dumps({"index": {"_index": INDEX_LIBRE, "_id": "1"}}) + "\n"
             + json.dumps({"a": "x", "libre": "pas mappe", "o": {"z": 1},
                           "liste": [1, 2]}) + "\n")
    req = urllib.request.Request(
        base + "/_bulk?refresh=true", data=corps.encode(), method="POST",
        headers={"Content-Type": "application/x-ndjson"})
    urllib.request.urlopen(req).read()


def cas():
    """(libelle, chemin, corps) — le corps est celui d'un `_search`."""
    p = f"/{INDEX}/_search?size=10&sort=ord"
    out = []

    def q(libelle, corps, chemin=p):
        out.append((libelle, chemin, corps))

    # --- fields : la forme du bloc -----------------------------------------
    q("fields, noms simples",
      {"_source": False, "fields": ["titre", "tag", "n", "l", "b", "d"]})
    q("fields, champ jamais rempli", {"_source": False, "fields": ["jamais"]})
    q("fields, champ non mappe", {"_source": False, "fields": ["pas_mappe"]})
    q("fields, multi-field", {"_source": False, "fields": ["titre.keyword"]})
    q("fields, sous-champ d'objet",
      {"_source": False, "fields": ["client.ville", "client.cp"]})
    q("fields, objet parent seul", {"_source": False, "fields": ["client"]})
    q("fields, motif client.*", {"_source": False, "fields": ["client.*"]})
    q("fields, motif *", {"_source": False, "fields": ["*"]})
    q("fields, motif sans correspondance", {"_source": False, "fields": ["zzz*"]})
    q("fields, doublon du meme nom", {"_source": False, "fields": ["n", "n"]})
    q("fields, nom + motif", {"_source": False, "fields": ["n", "*"]})
    q("fields, liste vide", {"_source": False, "fields": []})
    q("fields, ordre et doublons du _source",
      {"_source": False, "fields": ["tag", "n", "f"]})
    q("fields, coercions", {"_source": False, "fields": ["tag", "n", "b", "d"]})
    q("fields, avec _source", {"fields": ["n"]})
    q("fields, sur un text", {"_source": False, "fields": ["titre"]})

    # --- fields : les metadonnees ------------------------------------------
    for meta in ["_id", "_index", "_version", "_score", "_routing",
                 "_ignored", "_type", "_doc"]:
        q(f"fields, metadonnee {meta}", {"_source": False, "fields": [meta]})

    # --- fields : nested ----------------------------------------------------
    q("fields, nested un champ", {"_source": False, "fields": ["lignes.ref"]})
    q("fields, nested deux champs",
      {"_source": False, "fields": ["lignes.ref", "lignes.q"]})
    q("fields, objet sous un nested",
      {"_source": False, "fields": ["lignes.sous.z"]})
    q("fields, nested motif", {"_source": False, "fields": ["lignes.*"]})
    q("fields, nested racine seule", {"_source": False, "fields": ["lignes"]})

    # --- fields : format ----------------------------------------------------
    q("fields, format yyyy-MM-dd",
      {"_source": False, "fields": [{"field": "d", "format": "yyyy-MM-dd"}]})
    q("fields, format epoch_millis",
      {"_source": False, "fields": [{"field": "d", "format": "epoch_millis"}]})
    q("fields, format du mapping", {"_source": False, "fields": ["dfmt"]})
    q("fields, format qui ecrase celui du mapping",
      {"_source": False,
       "fields": [{"field": "dfmt", "format": "strict_date_optional_time"}]})
    q("fields, format sur un keyword",
      {"_source": False, "fields": [{"field": "tag", "format": "yyyy"}]})
    q("fields, format sur un motif qui attrape un keyword",
      {"_source": False, "fields": [{"field": "t*", "format": "yyyy"}]})

    # --- fields : include_unmapped ------------------------------------------
    q("fields, include_unmapped booleen",
      {"_source": False, "fields": [{"field": "*", "include_unmapped": True}]})
    q("fields, include_unmapped chaine",
      {"_source": False, "fields": [{"field": "n", "include_unmapped": "true"}]})

    # --- fields : les fautes de forme ---------------------------------------
    q("fields, chaine au lieu d'une liste", {"_source": False, "fields": "tag"})
    q("fields, null", {"_source": False, "fields": None})
    q("fields, objet sans field", {"fields": [{"format": "yyyy"}]})
    q("fields, objet a cle inconnue", {"fields": [{"field": "d", "truc": 1}]})

    # --- docvalue_fields ----------------------------------------------------
    q("dv, noms simples",
      {"_source": False, "docvalue_fields": ["tag", "n", "l", "f", "dbl", "b", "d"]})
    q("dv, tri et dedoublonnage", {"_source": False, "docvalue_fields": ["tag", "n", "f"]})
    q("dv, multi-field", {"_source": False, "docvalue_fields": ["titre.keyword"]})
    q("dv, sous-champ d'objet", {"_source": False, "docvalue_fields": ["client.ville"]})
    q("dv, motif client.*", {"_source": False, "docvalue_fields": ["client.*"]})
    q("dv, sous un nested", {"_source": False, "docvalue_fields": ["lignes.ref"]})
    q("dv, champ non mappe", {"_source": False, "docvalue_fields": ["pas_mappe"]})
    q("dv, champ jamais rempli", {"_source": False, "docvalue_fields": ["jamais"]})
    q("dv, sur un text", {"_source": False, "docvalue_fields": ["titre"]})
    q("dv, motif qui attrape un text", {"_source": False, "docvalue_fields": ["t*"]})
    q("dv, format sur une date",
      {"_source": False, "docvalue_fields": [{"field": "d", "format": "yyyy-MM-dd"}]})
    q("dv, format du mapping", {"_source": False, "docvalue_fields": ["dfmt"]})
    q("dv, garde le _source", {"docvalue_fields": ["n"]})
    q("dv, chaine au lieu d'une liste", {"_source": False, "docvalue_fields": "n"})
    q("dv, en query string", None, f"/{INDEX}/_search?size=10&sort=ord"
                                  "&docvalue_fields=n&_source=false")

    # --- stored_fields ------------------------------------------------------
    q("sf, une liste de noms", {"stored_fields": ["titre", "n"]})
    q("sf, une liste avec _source explicite",
      {"stored_fields": ["titre"], "_source": True})
    q("sf, motif *", {"stored_fields": ["*"]})
    q("sf, liste vide", {"stored_fields": []})
    q("sf, _none_", {"stored_fields": "_none_"})
    q("sf, _none_ + docvalue_fields",
      {"stored_fields": "_none_", "docvalue_fields": ["n"]})
    q("sf, _none_ + fields", {"stored_fields": "_none_", "fields": ["n"]})
    q("sf, en query string", None,
      f"/{INDEX}/_search?size=10&sort=ord&stored_fields=titre")
    q("sf, _none_ en query string", None,
      f"/{INDEX}/_search?size=10&sort=ord&stored_fields=_none_")

    # --- les trois ensemble, et les objets vides ----------------------------
    q("les trois ensemble", {"_source": ["n"], "fields": ["tag"],
                             "docvalue_fields": ["n"], "stored_fields": ["titre"]})
    q("script_fields vide", {"_source": False, "script_fields": {}})
    q("runtime_mappings vide", {"_source": False, "runtime_mappings": {}})
    q("runtime_mappings vide + fields",
      {"_source": False, "runtime_mappings": {}, "fields": ["n"]})

    # --- include_unmapped, la ou il a quelque chose a rendre -----------------
    libre = f"/{INDEX_LIBRE}/_search?size=10"
    q("libre, motif * sans include_unmapped",
      {"_source": False, "fields": ["*"]}, libre)
    q("libre, motif * avec include_unmapped",
      {"_source": False, "fields": [{"field": "*", "include_unmapped": True}]},
      libre)
    q("libre, nom precis avec include_unmapped",
      {"_source": False,
       "fields": [{"field": "libre", "include_unmapped": True}]}, libre)
    q("libre, nom precis sans include_unmapped",
      {"_source": False, "fields": ["libre"]}, libre)
    q("libre, motif o.* avec include_unmapped",
      {"_source": False, "fields": [{"field": "o.*", "include_unmapped": True}]},
      libre)
    q("libre, docvalue_fields sur un non mappe",
      {"_source": False, "docvalue_fields": ["libre"]}, libre)

    # --- sur un scroll : la page suivante doit transporter la meme chose ----
    q("fields sous scroll", {"_source": False, "fields": ["n", "tag"]},
      f"/{INDEX}/_search?size=2&sort=ord&scroll=1m")
    return out


def normalise(hit):
    """Ce qui se compare dans un hit : tout sauf le score et le tri."""
    return {k: v for k, v in hit.items() if k not in ("_score", "sort")}


def interroge(base, chemin, corps):
    """(ce qui se compare, ce qui s'affiche).

    Sur une erreur, seuls le **statut** et le type de la cause racine se
    comparent : ES empile ses erreurs de shard sous un
    `search_phase_execution_exception`, ferrite rend souvent l'erreur
    directement. C'est une divergence connue (`docs/compat.md`), pas un effet
    de ces parametres.
    """
    st, body = http(base, "POST", chemin, corps)
    if st != 200:
        e = body.get("error", {})
        if isinstance(e, dict):
            ty = e.get("root_cause", [{}])[0].get("type") or e.get("type", "?")
        else:
            ty = "?"
        return f"{st}", f"{st} {ty}"
    hits = [normalise(h) for h in body["hits"]["hits"]]
    vu = json.dumps(hits, sort_keys=True, ensure_ascii=False)
    return vu, vu[:150]


def main():
    ferrite = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:9200"
    es = sys.argv[2] if len(sys.argv) > 2 else "http://localhost:9201"
    detail = "--detail" in sys.argv
    cibles = [("ferrite", ferrite), ("es", es)]
    dispo = []
    for nom, base in cibles:
        try:
            http(base, "GET", "/")
            prepare(base)
            dispo.append((nom, base))
        except Exception as exc:  # noqa: BLE001
            print(f"# {nom} indisponible ({base}) : {exc}")
    ecarts = total = 0
    for libelle, chemin, corps in cas():
        reps = [(nom, *interroge(base, chemin, corps)) for nom, base in dispo]
        vals = {cle for _, cle, _ in reps}
        differe = len(vals) > 1
        print(f"{'*' if differe else ' '} {libelle:48} " +
              "  |  ".join(f"{nom}={vu}" for nom, _, vu in reps))
        if differe or detail:
            for nom, cle, _ in reps:
                print(f"      {nom}: {cle}")
        total += 1
        ecarts += differe
    print(f"\n{total - ecarts}/{total} identiques")
    return 1 if ecarts else 0


if __name__ == "__main__":
    sys.exit(main())
