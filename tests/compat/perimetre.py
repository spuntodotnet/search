#!/usr/bin/env python3
"""Rattacher un cas echoue de la suite d'Elastic au perimetre declare.

    python3 tests/compat/perimetre.py                       # l'index, tel qu'il est lu
    python3 tests/compat/perimetre.py search "unknown query [intervals] ..."

# Pourquoi

Le rapport de conformance comptait ses echecs sans savoir ce qu'ils disaient.
Un echec sur `_snapshot` et un echec sur `_search` y pesaient pareil, alors que
le premier est le prix d'un perimetre qu'on a **choisi** et le second un defaut
de ce qu'on **annonce**. Le taux qui en sortait etait donc un chiffre qu'on
subit, pas un chiffre qu'on pilote.

[`compat.yaml`](../../compat.yaml) declare le perimetre ; ce module s'en sert
pour trancher, cas par cas :

  regression      la capacite exercee est declaree `supporte` (ou `partiel` sur
                  une partie supportee) — c'est un vrai ecart, il compte
  cout_perimetre  la capacite est declaree `refuse` — attendu, et c'est le prix
                  affiche du perimetre
  indetermine     aucune capacite ne reclame ce cas. **Compte contre nous**,
                  comme une regression : un trou dans la declaration ne doit
                  jamais flatter le taux, sinon oublier de declarer une
                  capacite deviendrait payant

# Comment un cas est rattache

Dans cet ordre, et l'ordre compte :

1. un **motif d'erreur** declare par une capacite (`conformance.erreurs`) —
   c'est ce qui distingue « la route repond, mais la clause est refusee »
   (`unknown query [intervals]` sur `_search`) du reste. Ces motifs sont
   volontairement etroits : un motif large masquerait une vraie regression ;
2. l'**API** appelee par le `do` qui a echoue (`conformance.apis`), d'abord
   exacte, puis par famille (`snapshot.*`). Une API ne peut etre declaree que
   par une seule capacite — `genere_compat.py` refuse le contraire, sans quoi
   l'attribution dependrait de l'ordre de lecture du fichier ;
3. si la capacite trouvee est `partiel`, le message est relu : s'il nomme un
   parametre declare refuse, c'est un cout de perimetre, sinon une regression.

Un **marqueur** court-circuite les trois : c'est un fait constate sur la requete
elle-meme, pas devine sur son message. Le seul aujourd'hui est `api_typee` — le
cas a demande une URL avec un `{type}`, ce que la 8.x n'a plus. Le deduire du
message serait impossible : `no handler found for uri [/logs-1/test/1]` ne se
distingue pas d'une route manquante.
"""
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import genere_compat  # noqa: E402

REGRESSION = "regression"
COUT = "cout_perimetre"
INDETERMINE = "indetermine"

# Un parametre est reconnu **entre crochets**, comme Elasticsearch les nomme
# dans ses messages (« contains unrecognized parameter: [help] »). Le chercher
# en toutes lettres ferait des faux positifs sur les noms courts — `h`, `s`,
# `ts` apparaissent dans n'importe quelle phrase.
ENTRE_CROCHETS = "[{}]"


class Perimetre:
    """L'index de compat.yaml, du point de vue d'un cas echoue."""

    def __init__(self, doc=None):
        self.doc = doc or genere_compat.charge()
        self.capacites = {}
        self.apis = {}
        self.familles = []
        self.erreurs = []
        self.marqueurs = {}
        for _, cap in genere_compat.capacites(self.doc):
            self.capacites[cap["id"]] = cap
            conf = cap.get("conformance") or {}
            for api in conf.get("apis") or []:
                if api.endswith("*"):
                    self.familles.append((api[:-1], cap["id"]))
                else:
                    self.apis[api] = cap["id"]
            for motif in conf.get("erreurs") or []:
                self.erreurs.append((re.compile(motif), cap["id"]))
            for marqueur in conf.get("marqueurs") or []:
                self.marqueurs[marqueur] = cap["id"]
        # La famille la plus longue gagne : `cluster.health` est declaree
        # ailleurs, mais `cluster.*` ne doit pas prendre le pas sur un prefixe
        # plus precis qui apparaitrait un jour.
        self.familles.sort(key=lambda f: -len(f[0]))

    # -- rattachement --------------------------------------------------------

    def capacite_de(self, api, raison):
        for motif, cid in self.erreurs:
            if motif.search(raison or ""):
                return cid, "erreur"
        if api:
            if api in self.apis:
                return self.apis[api], "api"
            for prefixe, cid in self.familles:
                if api.startswith(prefixe):
                    return cid, "famille"
        return None, None

    def parametre_refuse(self, cap, raison):
        """Le message nomme-t-il, entre crochets, un parametre que cette
        capacite refuse ?"""
        for p in (cap.get("parametres") or {}).get("refuses") or []:
            nom = p if isinstance(p, str) else p.get("nom")
            if nom and ENTRE_CROCHETS.format(nom) in (raison or ""):
                return nom
        return None

    def verdict(self, api, raison, marqueurs=()):
        """(verdict, id de capacite, comment elle a ete trouvee).

        Un cas rattache par un **motif d'erreur** est un cout de perimetre quel
        que soit l'etat de la capacite : declarer le motif, c'est declarer que
        ce message-la est un refus assume. C'est le cas de
        `(transient|persistent) setting [...], not recognized`, pose sur une
        capacite `partiel` — la route existe, ce reglage-la est refuse.
        """
        for marqueur in marqueurs:
            if marqueur in self.marqueurs:
                return COUT, self.marqueurs[marqueur], "marqueur"
        cid, comment = self.capacite_de(api, raison)
        if cid is None:
            return INDETERMINE, None, None
        cap = self.capacites[cid]
        if comment == "erreur" or cap["etat"] == "refuse":
            return COUT, cid, comment
        if cap["etat"] == "partiel" and self.parametre_refuse(cap, raison):
            return COUT, cid, comment
        return REGRESSION, cid, comment


def main():
    perimetre = Perimetre()
    if len(sys.argv) > 1:
        api = sys.argv[1]
        raison = sys.argv[2] if len(sys.argv) > 2 else ""
        verdict, cid, comment = perimetre.verdict(api, raison)
        print(json.dumps({"api": api, "raison": raison, "verdict": verdict,
                          "capacite": cid, "trouvee_par": comment},
                         ensure_ascii=False, indent=2))
        return 0
    print(f"== {len(perimetre.capacites)} capacites, "
          f"{len(perimetre.apis)} API declarees, "
          f"{len(perimetre.familles)} familles, "
          f"{len(perimetre.erreurs)} motifs d'erreur")
    for api, cid in sorted(perimetre.apis.items()):
        print(f"  {api:<34} {cid}")
    for prefixe, cid in perimetre.familles:
        print(f"  {prefixe + '*':<34} {cid}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
