#!/usr/bin/env python3
"""La grille de scoring d'Elasticsearch — calculee par Elasticsearch.

    python3 tests/compat/genere_scoring.py           # ecrit tests/donnees/scoring.jsonl
    python3 tests/compat/genere_scoring.py --verifie  # regenere et compare

`function_score` pose un probleme que les autres clauses ne posent pas : son
resultat n'est pas un ensemble de documents ni un ordre, c'est une **valeur**.
Une formule recopiee depuis la documentation d'Elastic rend un nombre plausible,
et un nombre plausible ne se distingue pas d'un nombre juste par la lecture.

La reponse est celle de la carte 13, appliquee a une autre classe : le conteneur
de reference embarque un JDK **et** les jars d'ES.
`java -cp '/usr/share/elasticsearch/lib/*'` fait donc tourner, telles quelles :

  - `GaussDecayFunctionBuilder$GaussScoreFunction` et ses deux soeurs
    (`processScale`, puis `evaluate`) — les trois fonctions de decroissance ;
  - `FieldValueFactorFunction$Modifier` (`apply`) — les dix modificateurs ;
  - `CombineFunction` (`combine`) — les six `boost_mode`, plafond `max_boost`
    compris.

Ce que ce fichier ecrit est donc la reponse d'ES lui-meme, pas une idee de sa
reponse — et `tests/scoring_vs_es.rs` la rejoue dans `cargo test`, sans Docker.
C'est ce qui evite d'avoir a choisir une tolerance sur des flottants : il n'y a
pas d'ecart a tolerer, il y a un `f64` a rendre a l'identique.

La grille ne balaie pas au hasard : elle fait varier l'**echelle**, le **offset**
et le **decay**, et pose les valeurs la ou la formule se decide — sur le origin,
sur le bord du offset (a un ULP pres des deux cotes), sur `scale`, sur `2*scale`,
et tres loin, la ou `gauss` sous-passe a zero et ou `linear` a deja plafonne.
"""

import json
import pathlib
import subprocess
import sys

RACINE = pathlib.Path(__file__).resolve().parents[2]
GRILLE = RACINE / "tests/donnees/scoring.jsonl"
IMAGE = "docker.elastic.co/elasticsearch/elasticsearch:8.15.0"


GENERATEUR = r"""
import org.elasticsearch.index.query.functionscore.DecayFunction;
import org.elasticsearch.common.lucene.search.function.CombineFunction;
import org.elasticsearch.common.lucene.search.function.FieldValueFactorFunction;
import java.lang.reflect.Constructor;
import java.util.*;

/** La grille de scoring, calculee par les classes d'Elasticsearch elles-memes. */
public class Scoring {

    /** Les trois fonctions de decroissance, instanciees par reflexion : leurs
     *  classes sont finales et package-private, leurs methodes sont publiques. */
    static DecayFunction fonction(String nom) throws Exception {
        String cls = switch (nom) {
            case "gauss" -> "org.elasticsearch.index.query.functionscore."
                + "GaussDecayFunctionBuilder$GaussScoreFunction";
            case "exp" -> "org.elasticsearch.index.query.functionscore."
                + "ExponentialDecayFunctionBuilder$ExponentialDecayScoreFunction";
            case "linear" -> "org.elasticsearch.index.query.functionscore."
                + "LinearDecayFunctionBuilder$LinearDecayScoreFunction";
            default -> throw new IllegalArgumentException(nom);
        };
        Constructor<?> k = Class.forName(cls).getDeclaredConstructor();
        k.setAccessible(true);
        return (DecayFunction) k.newInstance();
    }

    static final String[] FONCTIONS = {"gauss", "exp", "linear"};
    /** Les echelles : l'unite, une echelle fine, une grosse, et 10 jours en
     *  millisecondes — celle d'un `gauss` sur un champ `date`. */
    static final double[] ECHELLES = {1.0, 2.5, 10.0, 0.001, 50.0, 864000000.0};
    /** `decay` : le defaut, les deux bords ou la formule devient raide ou plate. */
    static final double[] DECAYS = {0.5, 0.1, 0.9, 1.0 / 3.0, 0.999, 0.001};
    static final double[] OFFSETS = {0.0, 1.0, 10.0, 2.5};
    /** Les origines : zero, un nombre negatif, un grand entier, et un instant
     *  en millisecondes (2026-08-31), qui est le cas d'un champ `date`. */
    static final double[] ORIGINES = {0.0, -7.5, 1000000.0, 1788134400000.0};

    public static void main(String[] a) throws Exception {
        StringBuilder out = new StringBuilder();
        for (String nom : FONCTIONS) {
            DecayFunction f = fonction(nom);
            for (double echelle : ECHELLES)
                for (double decay : DECAYS)
                    for (double offset : OFFSETS)
                        for (double origine : ORIGINES)
                            decroissance(out, nom, f, origine, echelle, decay, offset);
        }
        for (FieldValueFactorFunction.Modifier m : FieldValueFactorFunction.Modifier.values())
            modificateur(out, m);
        for (CombineFunction c : CombineFunction.values())
            combinaison(out, c);
        System.out.print(out);
    }

    /** Les valeurs ou la decroissance se decide, pour ce couple offset/echelle. */
    static double[] valeurs(double origine, double echelle, double offset) {
        List<Double> v = new ArrayList<>();
        double[] ecarts = {
            0.0, Math.ulp(offset + origine), offset, Math.nextUp(offset), Math.nextDown(offset),
            offset + echelle, offset + 2 * echelle, offset + 10 * echelle,
            echelle / 2, echelle, 2 * echelle, 3 * echelle, 100 * echelle,
            // Assez loin pour que `gauss` sous-passe a zero et que la
            // difference `valeur - origine` deborde.
            1e300, Double.POSITIVE_INFINITY,
        };
        for (double e : ecarts) {
            v.add(origine + e);
            v.add(origine - e);
        }
        double[] out = new double[v.size()];
        for (int i = 0; i < out.length; i++) out[i] = v.get(i);
        return out;
    }

    static void decroissance(StringBuilder out, String nom, DecayFunction f,
                             double origine, double echelle, double decay, double offset) {
        double param = f.processScale(echelle, decay);
        out.append("{\"t\":\"decroissance\",\"f\":\"").append(nom)
           .append("\",\"o\":").append(nb(origine))
           .append(",\"s\":").append(nb(echelle))
           .append(",\"d\":").append(nb(decay))
           .append(",\"e\":").append(nb(offset))
           .append(",\"p\":").append(nb(param))
           .append(",\"c\":[");
        double[] valeurs = valeurs(origine, echelle, offset);
        for (int i = 0; i < valeurs.length; i++) {
            // La distance est exactement celle que calcule
            // `DecayFunctionBuilder.NumericFieldDataScoreFunction` :
            // `max(0, |valeur - origine| - offset)`.
            double distance = Math.max(0.0, Math.abs(valeurs[i] - origine) - offset);
            if (i > 0) out.append(',');
            out.append('[').append(nb(valeurs[i])).append(',').append(nb(distance))
               .append(',').append(nb(f.evaluate(distance, param))).append(']');
        }
        out.append("]}\n");
    }

    /** Les valeurs ou un modificateur se decide : les bords de son domaine. */
    static final double[] VALEURS_MOD = {
        0.0, 1.0, 2.0, 3.0, 0.5, 1e-9, 1e9, 1e-300, 1e300, 10.0, 100.0,
        -1.0, -0.5, -2.0, 0.25, 1.5, 7.0, 1788134400000.0, Math.E, Math.PI,
        Math.nextUp(0.0), Math.nextDown(1.0), Math.nextUp(1.0), 4.9E-324,
        // Ce qu'un `missing` negatif et un `factor` font arriver dans un
        // modificateur, et que la premiere grille n'avait pas.
        Double.NaN, Double.POSITIVE_INFINITY, Double.NEGATIVE_INFINITY,
        -1e300, -1.5, -3.0,
    };

    static void modificateur(StringBuilder out, FieldValueFactorFunction.Modifier m) {
        out.append("{\"t\":\"modificateur\",\"m\":\"").append(m).append("\",\"c\":[");
        for (int i = 0; i < VALEURS_MOD.length; i++) {
            if (i > 0) out.append(',');
            out.append('[').append(nb(VALEURS_MOD[i])).append(',')
               .append(nb(m.apply(VALEURS_MOD[i]))).append(']');
        }
        out.append("]}\n");
    }

    // `NaN` et les infinis ne sont pas des curiosites : un `sqrt` sur une
    // valeur negative ou un `log1p` sous -1 en produisent, et c'est la que
    // `Math.min` de Java cesse d'etre `f64::min` de Rust — le premier propage
    // `NaN`, le second rend l'autre operande. Sans eux, la grille laissait
    // passer un score invente rendu en 200.
    static final double[] SCORES = {
        0.0, 1.0, 0.15965708, 2.5, 1e-9, 1e9, 12345.6789,
        Double.NaN, Double.POSITIVE_INFINITY, Double.NEGATIVE_INFINITY, -1.0,
    };
    static final double[] FACTEURS = {
        0.0, 1.0, 2.0, 0.5, 1e-30, 1e30, 3.7, 1e9,
        Double.NaN, Double.POSITIVE_INFINITY, Double.NEGATIVE_INFINITY, -2.0,
    };
    static final double[] PLAFONDS = {
        Float.MAX_VALUE, 1.0, 2.0, 0.5, 1e9, 0.0,
        Double.NaN, Double.POSITIVE_INFINITY,
    };

    static void combinaison(StringBuilder out, CombineFunction c) {
        out.append("{\"t\":\"combinaison\",\"b\":\"").append(c).append("\",\"c\":[");
        boolean premier = true;
        for (double q : SCORES)
            for (double fa : FACTEURS)
                for (double p : PLAFONDS) {
                    if (!premier) out.append(',');
                    premier = false;
                    // `combine` rend un `float` : c'est le seul endroit de la
                    // chaine ou ES quitte le `double`, et donc le seul endroit
                    // ou l'arrondi compte.
                    out.append('[').append(nb(q)).append(',').append(nb(fa)).append(',')
                       .append(nb(p)).append(',').append(nb(c.combine(q, fa, p))).append(']');
                }
        out.append("]}\n");
    }

    /** Tout nombre passe en **chaine**, et ce n'est pas une precaution de
     *  style : le parseur de flottants de `serde_json` se trompe d'un ULP sur
     *  `1000000.0000000001` (il rend le double d'en dessous), la ou le
     *  `str::parse::<f64>` de la bibliotheque standard est exact. Une grille
     *  qui perd un bit au passage ne mesure plus le bit qu'on veut mesurer.
     *  JSON n'a de toute facon ni `NaN` ni les infinis. */
    static String nb(double d) {
        if (Double.isNaN(d)) return "\"NaN\"";
        if (d == Double.POSITIVE_INFINITY) return "\"Infinity\"";
        if (d == Double.NEGATIVE_INFINITY) return "\"-Infinity\"";
        return "\"" + Double.toString(d) + "\"";
    }

    static String nb(float f) {
        return nb((double) f);
    }
}
"""


def conteneur():
    """Le conteneur de reference qui tourne, ou rien."""
    sortie = subprocess.run(
        ["docker", "ps", "--filter", f"ancestor={IMAGE}", "--format", "{{.Names}}"],
        capture_output=True,
        text=True,
    ).stdout.split()
    return sortie[0] if sortie else None


def genere():
    nom = conteneur()
    if not nom:
        print(f"!! aucun conteneur {IMAGE} ne tourne — l'oracle est dans ses jars")
        print("   docker run -d --name es-ref -p 9201:9200 \\")
        print("     -e discovery.type=single-node -e xpack.security.enabled=false \\")
        print(f"     {IMAGE}")
        sys.exit(2)
    java = pathlib.Path("/tmp/ferrite-Scoring.java")
    java.write_text(GENERATEUR, encoding="utf-8")
    subprocess.run(["docker", "cp", str(java), f"{nom}:/tmp/Scoring.java"], check=True)
    lignes = subprocess.run(
        ["docker", "exec", nom, "sh", "-c",
         "cd /tmp && /usr/share/elasticsearch/jdk/bin/java "
         "-cp '/usr/share/elasticsearch/lib/*' Scoring.java"],
        capture_output=True, text=True, check=True,
    ).stdout
    entete = (
        '{"_": "Genere par tests/compat/genere_scoring.py. Chaque ligne porte une '
        'batterie de scoring calculee par Elasticsearch 8.15.0 lui-meme, dans le '
        'conteneur de reference : les fonctions de decroissance '
        '(GaussDecayFunctionBuilder$GaussScoreFunction et ses soeurs), les '
        'modificateurs (FieldValueFactorFunction$Modifier) et les combinaisons '
        '(CombineFunction). Rejoue par tests/scoring_vs_es.rs."}\n'
    )
    return entete + lignes


def main():
    contenu = genere()
    cas = 0
    for ligne in contenu.splitlines():
        v = json.loads(ligne)
        cas += len(v.get("c", []))
    if "--verifie" in sys.argv[1:]:
        ancien = GRILLE.read_text(encoding="utf-8")
        if ancien != contenu:
            print(f"!! {GRILLE.relative_to(RACINE)} differe de ce que le conteneur rend")
            sys.exit(1)
        print(f"ok  {GRILLE.relative_to(RACINE)} est bien ce que le conteneur rend "
              f"({cas} points)")
        return
    GRILLE.parent.mkdir(parents=True, exist_ok=True)
    GRILLE.write_text(contenu, encoding="utf-8")
    print(f"ok  {len(contenu.splitlines()) - 1} batteries, {cas} points mesures")
    print(f"    ecrit {GRILLE.relative_to(RACINE)}")


if __name__ == "__main__":
    main()
