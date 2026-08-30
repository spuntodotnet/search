#!/usr/bin/env python3
"""Sonde : **ecrire** un alias — les sept URL, le corps qui remplace le chemin,
`must_exist`, et le 404 quand un `remove` ne designe rien.

`sonde_alias.py` mesure la **lecture** (`GET /_alias/{expression}`). Celle-ci
mesure l'ecriture, et elle existe parce que trois de ses reponses n'etaient pas
devinables — aucune n'est documentee, et la suite de conformance d'Elastic,
figee en 7.10.2, ne pouvait pas les exercer :

    PUT /{index}/_alias  {"alias": "a"}     le nom de l'alias vient du corps
    PUT /inconnu/_alias/a {"index": "reel"} le corps **remplace** le chemin
    remove {index: "wz*", alias: "ex1", must_exist: true}
                                            404 des qu'**un** des index visés ne
                                            porte pas l'alias — meme si un autre
                                            le porte
    remove {index: "wz1", alias: "absent"}  404 « aliases [absent] missing »,
                                            mais seulement si **toute** la
                                            requete finit sans rien faire

Les deux dernieres lignes sont la meme mesure sous deux regimes : `must_exist`
se verifie **par index**, le 404 par defaut est **global**. Ecrire l'un a la
place de l'autre rend un 200 la ou ES rend 404, et l'inverse.

Chaque cas remet les deux serveurs dans le meme etat, envoie la meme commande,
et compare trois choses : le statut, le message, et **l'etat laisse derriere**
(quel index porte quel alias). Une commande qui rend le bon statut en posant
l'alias ailleurs serait verte sur les deux premieres.

    python3 tests/compat/sonde_ecriture_alias.py [ferrite] [es]
    python3 tests/compat/sonde_ecriture_alias.py --calibrer [es_a] [es_b]

Le second mode est ce qui autorise a croire le premier : la batterie **ecrit**
sur les serveurs qu'elle interroge, elle ne peut donc pas s'etalonner contre un
seul. Tant qu'elle n'est pas a zero contre deux Elasticsearch, ce qu'elle dit de
ferrite ne vaut rien.
"""
import json
import re
import sys
import urllib.error
import urllib.request

A = "sonde-ecr-a"
B = "sonde-ecr-b"

# L'etat de depart de chaque cas : deux index, un seul alias, pose sur A.
DEPART = {A: ["ex1"], B: []}

# Ce que ferrite refuse **exprès** la ou ES repond. Chaque ligne porte sa
# mesure : ce qu'ES fait, et pourquoi le recopier serait pire que le refuser.
REFUS_ASSUMES = {
    "corps: indices pluriel":
        "ES ignore [indices] dans ce corps et rend « [indices] can't be empty » : "
        "la cle est lue par personne, mais le message accuse la bonne",
    "corps: aliases pluriel":
        "meme chose pour [aliases] : ES rend « [alias] can't be empty string »",
    "corps: index en liste JSON":
        "ES n'en garde que le **dernier** element, en 200 — l'alias part alors "
        "ailleurs que la ou le corps le demande, sans un mot",
    "corps: alias en liste JSON":
        "idem : ES ne garde que le dernier nom, en 200",
    "corps: filter":
        "un alias filtre dont le filtre n'est pas applique rendrait precisement "
        "les documents qu'il est cense cacher — refus deja declare sur "
        "`POST /_aliases`, et il doit l'etre de la meme facon ici",
    "corps: routing":
        "ferrite est mono-shard : le routage n'a rien a choisir. Meme refus que "
        "sur `POST /_aliases`, au meme endroit du code",
    "corps: is_hidden":
        "un alias cache change ce que `_all` et les jokers designent ; l'accepter "
        "sans le tenir ferait remonter des index qu'il doit taire",
}

CAS = [
    # ---- les sept URL de put_alias -------------------------------------
    ("PUT /{index}/_alias/{nom}", "PUT", f"/{A}/_alias/n1", None),
    ("POST /{index}/_alias/{nom}", "POST", f"/{A}/_alias/n1", None),
    ("PUT /{index}/_aliases/{nom}", "PUT", f"/{A}/_aliases/n1", None),
    ("PUT /{index}/_alias + alias au corps", "PUT", f"/{A}/_alias", {"alias": "n1"}),
    ("PUT /{index}/_aliases + alias au corps", "PUT", f"/{A}/_aliases", {"alias": "n1"}),
    ("PUT /_alias/{nom} + index au corps", "PUT", "/_alias/n1", {"index": A}),
    ("POST /_alias/{nom} + index au corps", "POST", "/_alias/n1", {"index": A}),
    ("PUT /_aliases/{nom} + index au corps", "PUT", "/_aliases/n1", {"index": A}),
    ("PUT /_alias + les deux au corps", "PUT", "/_alias", {"index": A, "alias": "n1"}),
    # ---- le corps remplace le chemin -----------------------------------
    ("corps index remplace un chemin inconnu", "PUT", "/index-inconnu/_alias/n1",
     {"index": A}),
    ("corps alias remplace le nom du chemin", "PUT", f"/{A}/_alias/ignore",
     {"alias": "n1"}),
    ("corps index en liste separee par des virgules", "PUT", "/_alias/n1",
     {"index": f"{A},{B}"}),
    ("corps index en motif", "PUT", "/_alias/n1", {"index": "sonde-ecr-*"}),
    ("chemin index en liste, alias au corps", "PUT", f"/{A},{B}/_alias",
     {"alias": "n1"}),
    ("is_write_index avec l'alias au corps", "PUT", f"/{A}/_alias",
     {"alias": "n1", "is_write_index": True}),
    # ---- les refus de forme --------------------------------------------
    ("aucun index nulle part", "PUT", "/_alias", {"alias": "n1"}),
    ("aucun alias nulle part", "PUT", f"/{A}/_alias", None),
    ("corps vide sur l'URL complete", "PUT", f"/{A}/_alias/n1", {}),
    ("index inconnu au corps", "PUT", "/_alias/n1", {"index": "pas-la"}),
    ("nom d'alias avec un joker", "PUT", f"/{A}/_alias/mauvais*", None),
    ("nom d'alias avec une virgule", "PUT", f"/{A}/_alias/n1,n2", None),
    ("alias au corps avec une virgule", "PUT", f"/{A}/_alias", {"alias": "n1,n2"}),
    ("alias du nom d'un index", "PUT", f"/{A}/_alias/{B}", None),
    ("corps: indices pluriel", "PUT", "/_alias/n1", {"indices": [A]}),
    ("corps: aliases pluriel", "PUT", f"/{A}/_alias", {"aliases": ["n1"]}),
    ("corps: index en liste JSON", "PUT", "/_alias/n1", {"index": [A, B]}),
    ("corps: alias en liste JSON", "PUT", f"/{A}/_alias", {"alias": ["n1", "n2"]}),
    # Ce que ferrite refuse deja sur `POST /_aliases` doit l'etre ici de la
    # meme facon : une declaration d'alias se lit au meme endroit, quelle que
    # soit l'URL qui la porte.
    ("corps: filter", "PUT", f"/{A}/_alias", {"alias": "n1",
                                              "filter": {"match_all": {}}}),
    ("corps: routing", "PUT", f"/{A}/_alias", {"alias": "n1", "routing": "x"}),
    ("corps: is_hidden", "PUT", f"/{A}/_alias", {"alias": "n1", "is_hidden": True}),
    # ---- DELETE /{index}/_alias/{nom} ----------------------------------
    ("DELETE alias present", "DELETE", f"/{A}/_alias/ex1", None),
    ("DELETE alias absent", "DELETE", f"/{A}/_alias/absent", None),
    ("DELETE alias present sur un autre index", "DELETE", f"/{B}/_alias/ex1", None),
    ("DELETE motif qui correspond", "DELETE", f"/{A}/_alias/e*", None),
    ("DELETE motif qui ne correspond a rien", "DELETE", f"/{B}/_alias/e*", None),
    ("DELETE _all sur un index qui a un alias", "DELETE", f"/{A}/_alias/_all", None),
    ("DELETE _all sur un index sans alias", "DELETE", f"/{B}/_alias/_all", None),
    ("DELETE liste dont un terme porte", "DELETE", f"/{A}/_alias/ex1,absent", None),
    ("DELETE sur une liste d'index", "DELETE", f"/{A},{B}/_alias/ex1", None),
    ("DELETE sur un motif d'index", "DELETE", "/sonde-ecr-*/_alias/ex1", None),
    # ---- POST /_aliases : must_exist et le 404 global ------------------
    ("remove present", "POST", "/_aliases",
     {"actions": [{"remove": {"index": A, "alias": "ex1"}}]}),
    ("remove absent, defaut", "POST", "/_aliases",
     {"actions": [{"remove": {"index": A, "alias": "absent"}}]}),
    ("remove absent, must_exist=true", "POST", "/_aliases",
     {"actions": [{"remove": {"index": A, "alias": "absent", "must_exist": True}}]}),
    ("remove absent, must_exist=false", "POST", "/_aliases",
     {"actions": [{"remove": {"index": A, "alias": "absent", "must_exist": False}}]}),
    ("remove joker sans correspondance, defaut", "POST", "/_aliases",
     {"actions": [{"remove": {"index": A, "aliases": ["zz*"]}}]}),
    ("remove joker sans correspondance, must_exist=true", "POST", "/_aliases",
     {"actions": [{"remove": {"index": A, "aliases": ["zz*"], "must_exist": True}}]}),
    ("remove joker qui correspond", "POST", "/_aliases",
     {"actions": [{"remove": {"index": A, "aliases": ["ex*"]}}]}),
    ("remove [present, absent], defaut", "POST", "/_aliases",
     {"actions": [{"remove": {"index": A, "aliases": ["ex1", "absent"]}}]}),
    ("remove [present, absent], must_exist=true", "POST", "/_aliases",
     {"actions": [{"remove": {"index": A, "aliases": ["ex1", "absent"],
                              "must_exist": True}}]}),
    ("remove [absent, absent], must_exist=true", "POST", "/_aliases",
     {"actions": [{"remove": {"index": A, "aliases": ["ab1", "ab2"],
                              "must_exist": True}}]}),
    ("remove sur un motif d'index, un seul porte, defaut", "POST", "/_aliases",
     {"actions": [{"remove": {"index": "sonde-ecr-*", "alias": "ex1"}}]}),
    ("remove sur un motif d'index, un seul porte, must_exist=true", "POST", "/_aliases",
     {"actions": [{"remove": {"index": "sonde-ecr-*", "alias": "ex1",
                              "must_exist": True}}]}),
    ("remove sur indices [A,B], un seul porte, must_exist=true", "POST", "/_aliases",
     {"actions": [{"remove": {"indices": [A, B], "alias": "ex1",
                              "must_exist": True}}]}),
    ("remove _all sur un index qui a un alias", "POST", "/_aliases",
     {"actions": [{"remove": {"index": A, "alias": "_all"}}]}),
    ("remove _all sur un index sans alias", "POST", "/_aliases",
     {"actions": [{"remove": {"index": B, "alias": "_all"}}]}),
    ("remove absent + add valide : le 404 est global", "POST", "/_aliases",
     {"actions": [{"remove": {"index": A, "alias": "absent"}},
                  {"add": {"index": B, "alias": "n1"}}]}),
    ("deux remove tous absents", "POST", "/_aliases",
     {"actions": [{"remove": {"index": A, "alias": "ab1"}},
                  {"remove": {"index": A, "alias": "ab2"}}]}),
    ("remove aliases: []", "POST", "/_aliases",
     {"actions": [{"remove": {"index": A, "aliases": []}}]}),
    ("remove sans alias du tout", "POST", "/_aliases",
     {"actions": [{"remove": {"index": A}}]}),
    ("remove sans index", "POST", "/_aliases",
     {"actions": [{"remove": {"alias": "ex1"}}]}),
    ("must_exist pas booleen", "POST", "/_aliases",
     {"actions": [{"remove": {"index": A, "alias": "ex1", "must_exist": "oui"}}]}),
    ("must_exist sur un add", "POST", "/_aliases",
     {"actions": [{"add": {"index": A, "alias": "n1", "must_exist": True}}]}),
    ("add avec un joker dans le nom", "POST", "/_aliases",
     {"actions": [{"add": {"index": A, "alias": "mauvais*"}}]}),
    ("add sur un motif d'index", "POST", "/_aliases",
     {"actions": [{"add": {"index": "sonde-ecr-*", "alias": "n1"}}]}),
    ("actions vide", "POST", "/_aliases", {"actions": []}),
]

MARQUEUR_FERRITE = "not_implemented_in_ferrite_exception"


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


def prepare(base):
    """Le meme etat de depart des deux cotes, a chaque cas."""
    for nom in (A, B):
        http(base, "DELETE", f"/{nom}")
    for nom, alias in DEPART.items():
        statut, corps = http(base, "PUT", f"/{nom}",
                             {"aliases": {a: {} for a in alias}})
        if statut >= 400:
            print(f"[{base}] ne prend pas l'index de la sonde : {corps}",
                  file=sys.stderr)
            sys.exit(2)


def etat(base):
    """Quel index porte quel alias — l'etat qu'un statut seul ne dit pas."""
    _, corps = http(base, "GET", f"/{A},{B}/_alias")
    if not isinstance(corps, dict):
        return {}
    return {i: sorted(v.get("aliases", {})) for i, v in corps.items()
            if not isinstance(v, (str, int))}


def raison(corps):
    """Le type de tete, et **tout** le corps de l'erreur.

    ES range souvent la phrase utile sous `caused_by` et met un enrobage de
    parseur en tete (« [1:62] [aliases] failed to parse field [actions] »). Ne
    lire que le `reason` du dessus ferait passer deux messages identiques pour
    deux messages differents — c'est d'ailleurs le corps entier que compare le
    `catch: /…/` d'une suite de conformance.
    """
    err = corps.get("error") if isinstance(corps, dict) else None
    ty = err.get("type") if isinstance(err, dict) else None
    return ty, json.dumps(err, ensure_ascii=False) if err else ""


def joue(base, methode, chemin, corps):
    prepare(base)
    statut, reponse = http(base, methode, chemin, corps)
    ty, msg = raison(reponse)
    return statut, ty, msg, etat(base)


def comparable(msg):
    """Un message se compare sur sa **forme**, pas sur son texte.

    Les deux serveurs n'ecrivent pas leurs refus dans la meme langue ; ce qui
    doit coincider, c'est le nom de la ressource qu'ES cite — c'est lui que les
    clients et les suites de tests grepent.
    """
    trouve = re.search(r"aliases \[([^\]]*)\] missing", msg or "")
    if trouve:
        # Un seul ecart survit ici, et il porte sur l'**ordre** : quand plusieurs
        # noms manquent, ES les rend dans l'ordre d'iteration d'un `HashSet` de
        # Java (`[ab1, ab2]` ecrit rend `[ab2, ab1]`), qui n'est l'ordre de rien.
        # ferrite rend l'ordre ecrit. Les noms sont donc compares comme un
        # ensemble — mais compares : en manquer un resterait un ecart.
        noms = ",".join(sorted(n.strip() for n in trouve.group(1).split(",")))
        return f"aliases [{noms}] missing"
    for motif in (
                  r"\[alias(es)?\] can't be empty( string)?",
                  r"\[indices\] can't be empty",
                  r"One of \[\w+\] or \[\w+\] is required",
                  r"no such index",
                  r"No action specified",
                  r"as only \[true\] or \[false\] are allowed",
                  r"Invalid alias name",
                  r"an index or data stream exists with the same name as the alias"):
        trouve = re.search(motif, msg or "")
        if trouve:
            return trouve.group(0)
    return ""


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    calibrer = "--calibrer" in sys.argv
    defauts = ("http://localhost:9201", "http://localhost:9202") if calibrer else (
        "http://localhost:9200", "http://localhost:9201")
    ferrite = args[0] if args else defauts[0]
    es = args[1] if len(args) > 1 else defauts[1]
    for base, nom in ((ferrite, "es_a" if calibrer else "ferrite"), (es, "es_b" if calibrer else "es")):
        statut, _ = http(base, "GET", "/")
        if statut != 200:
            print(f"[{nom}] ne repond pas sur {base} — une sonde qui ne trouve "
                  "qu'un serveur ne compare rien", file=sys.stderr)
            return 2

    ecarts = refus = identiques = 0
    sans_motif = []
    for label, methode, chemin, corps in CAS:
        sa, ta, ma, ea = joue(ferrite, methode, chemin, corps)
        sb, _, mb, eb = joue(es, methode, chemin, corps)
        if ta == MARQUEUR_FERRITE and label in REFUS_ASSUMES:
            refus += 1
            print(f"refus {label}\n      {REFUS_ASSUMES[label]}")
            continue
        ca, cb = comparable(ma), comparable(mb)
        # Un message qu'aucun motif ne reconnait des **deux** cotes n'est pas
        # « identique » : il n'est pas compare. Le compter vert reviendrait a
        # faire d'un predicat muet un predicat qui approuve — c'est exactement
        # le genre de vert flatteur que ce depot chasse. Il est donc sorti a
        # part, et imprime.
        if sa == sb and ea == eb and ma and mb and not ca and not cb:
            sans_motif.append((label, ma[:110], mb[:110]))
            print(f"?msg  {label}")
            continue
        if sa == sb and ca == cb and ea == eb:
            identiques += 1
            print(f"ok    {label}")
            continue
        ecarts += 1
        print(f"ECART {label}")
        print(f"      ferrite {sa} {ca or ma[:90]!r} {ea}")
        print(f"      es      {sb} {cb or mb[:90]!r} {eb}")

    for base in (ferrite, es):
        for nom in (A, B):
            http(base, "DELETE", f"/{nom}")

    if sans_motif:
        print("\nmessages non compares (meme statut, meme etat, aucun motif "
              "connu des deux cotes) :")
        for label, ma, mb in sans_motif:
            print(f"  {label}\n    ferrite {ma}\n    es      {mb}")

    print(f"\n{identiques}/{len(CAS)} identiques, {refus} refus assumes, "
          f"{len(sans_motif)} messages non compares, {ecarts} ecarts")
    return 1 if ecarts else 0


if __name__ == "__main__":
    sys.exit(main())
