#!/usr/bin/env python3
"""Sonde : que designe *vraiment* une expression de noms d'alias chez ES ?

`GET /_alias/{nom}` accepte une liste, des jokers et des exclusions. La
documentation dit que le tiret exclut ; elle ne dit rien de ce qui decide du
404, et c'est la que tout se joue :

    test_alias_1,-test                        404 sur [-test]
    test_blias_2,test_alias*,-test_alias_1    200

La meme exclusion d'un alias qui existe, une fois refusee et une fois acceptee.
Ce qui les separe est le **joker** : tant qu'aucun terme n'est un motif, ES
compare la liste **ecrite** a ce qu'il rend — une exclusion y figure telle
quelle, tiret compris, donc elle manque. Des qu'un motif apparait, la liste
ecrite cede la place a une liste **resolue**, ou ne restent que les noms qui ont
survecu aux exclusions.

Aucune de ces deux regles n'etait devinable, et la seconde contredit la
premiere. D'ou ce fichier : il pose les memes expressions aux deux serveurs et
imprime leurs deux reponses cote a cote.

    python3 tests/compat/sonde_alias.py [ferrite] [es]
"""
import json
import sys
import urllib.error
import urllib.request

INDEX = "sonde-alias"
ALIAS = ["test_alias_1", "test_alias_2", "test_blias_1", "test_blias_2", "test"]

# Chaque expression est choisie pour separer deux lectures possibles de la
# regle : ordre des termes, tiret en premiere position, exclusion avant ou
# apres un joker, exclusion d'un alias qui existe, d'un qui n'existe pas.
EXPRESSIONS = [
    "_all",
    "*",
    "test_alias_1,non_existent",
    "test_blias_2,test_alias*,-test_alias_1",
    "test_alias_1,test_blias_1,-test_alias*",
    "-test_alias_1,test_alias*,-test_alias_2",
    "-test_alias_1,-non-existing,test_alias*,-test",
    "test_alias*,-non-existent,test_blias*,-test",
    "test_alias_1,test_blias_2,-test_alias*,-test_blias_2",
    "-non-existent,-non-existent*,-another",
    "non_a,non_b",
    "nomatch*",
    "test_alias*,non_existent",
    "test_blias_1,-test_blias_1",
    "non_existent,test_alias*",
    "test_alias_1,-test_alias_1,non_existent",
    "*,non_existent",
    "test_alias_1,non_a,non_b",
    "-test",
    "-test,test_alias_1",
    "test_alias_1,-test",
]


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
    http(base, "DELETE", f"/{INDEX}")
    statut, corps = http(base, "PUT", f"/{INDEX}",
                         {"aliases": {a: {} for a in ALIAS}})
    if statut >= 400:
        print(f"[{base}] ne prend pas l'index de la sonde : {corps}",
              file=sys.stderr)
        sys.exit(2)


def interroge(base, expr):
    """Le statut, les alias rendus, et le message d'erreur — les trois comptent.

    Un 404 d'ES porte quand meme les alias trouves : rendre le bon corps avec le
    mauvais statut, ou l'inverse, sont deux echecs differents.
    """
    statut, corps = http(base, "GET", f"/_alias/{urllib.request.quote(expr, safe=',*')}")
    rendus = sorted(corps.get(INDEX, {}).get("aliases", {})) if isinstance(corps, dict) else []
    erreur = corps.get("error") if isinstance(corps, dict) else None
    if isinstance(erreur, dict):  # l'erreur objet des autres routes
        erreur = erreur.get("reason")
    return statut, rendus, erreur


def main():
    ferrite = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:9200"
    es = sys.argv[2] if len(sys.argv) > 2 else "http://localhost:9201"
    for base in (ferrite, es):
        prepare(base)

    ecarts = 0
    for expr in EXPRESSIONS:
        a = interroge(ferrite, expr)
        b = interroge(es, expr)
        meme = "ok " if a == b else "ECART"
        if a != b:
            ecarts += 1
        print(f"{meme} {expr}")
        for nom, (statut, rendus, erreur) in (("ferrite", a), ("es     ", b)):
            print(f"       {nom} {statut} {rendus}" + (f"  {erreur!r}" if erreur else ""))
    for base in (ferrite, es):
        http(base, "DELETE", f"/{INDEX}")

    print(f"\n{len(EXPRESSIONS) - ecarts}/{len(EXPRESSIONS)} expressions identiques")
    return 1 if ecarts else 0


if __name__ == "__main__":
    sys.exit(main())
