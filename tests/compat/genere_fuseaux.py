#!/usr/bin/env python3
"""La table des fuseaux horaires, prise dans le tzdb du **conteneur de reference**.

    python3 tests/compat/genere_fuseaux.py            # regenere src/tzdata.bin
    python3 tests/compat/genere_fuseaux.py --verifie  # le cliquet : rien n'a bouge ?
    python3 tests/compat/genere_fuseaux.py --grille   # la grille d'arrondis d'ES

Un `date_histogram` avec `time_zone` ne se calcule pas sans les regles de
changement d'heure : c'est le fuseau qui decide qu'un seau « par jour » dure 23
ou 25 heures. Reste a savoir **quelles** regles, et la reponse n'est pas « les
dernieres publiees » : Elasticsearch lit celles de son JDK
(`jdk/lib/tzdb.dat`, 104 Ko), pas celles du systeme — l'image d'ES n'a meme pas
de `/usr/share/zoneinfo`. Une table tiree d'ailleurs divergerait de l'arbitre
sur toute zone dont les regles ont bouge entre deux versions du tzdb, et elle
divergerait **en silence** : un seau decale d'une heure ressemble a un seau.

La table est donc dumpee du JDK que le conteneur de reference embarque, par un
programme Java de trente lignes qui interroge `java.time.zone.ZoneRules` — la
meme classe qu'ES appelle. C'est le geste de `sonde_langues.py --mots-vides`,
qui va lire les listes de mots vides dans le jar de Lucene du meme conteneur :
on ne recopie pas une source de verite, on la lit la ou le moteur la lit.

Ce que le fichier contient (mesure du 2024a) : 603 zones, 352 jeux de regles
distincts (les liens partagent leurs regles), 18 078 transitions historiques et
238 regles annuelles pour le futur.

`--verifie` redumpe et compare **octet par octet** au fichier commite, puis
relit ce fichier pour verifier que chaque zone du dump s'y retrouve a
l'identique — les deux sens, comme le relevé des mots vides.

`--grille` ecrit `tests/donnees/arrondis.jsonl` : une grille de couples
(zone, intervalle, instant) dont les reponses sont celles de la classe
**`org.elasticsearch.common.Rounding` d'Elasticsearch elle-meme**, appelee
dans le conteneur avec les jars d'ES au classpath. Ce n'est donc pas une
lecture de son code : c'est son code, execute. `tests/arrondi_vs_es.rs` la
rejoue dans `cargo test`, sans Docker — le meme geste que le spike de
`tests/spike_sous_aggs.rs`, mais avec l'arbitre pour oracle.

Les instants de la grille ne sont pas tires au hasard : pour chaque zone, ce
sont **ses** bascules — la premiere, les recentes, et celles que ses regles
annuelles produiront — entourees a la milliseconde, plus quelques dates fixes
(un 29 fevrier, un 1er janvier, une annee 2044 qu'aucune table ne porte). Une
grille reguliere passerait a cote des seuls endroits ou l'arrondi est
difficile.
"""
import pathlib
import subprocess
import sys

RACINE = pathlib.Path(__file__).resolve().parents[2]
BIN = RACINE / "src" / "tzdata.bin"
RS = RACINE / "src" / "tzdata.rs"
GRILLE = RACINE / "tests" / "donnees" / "arrondis.jsonl"
IMAGE = "docker.elastic.co/elasticsearch/elasticsearch:8.15.0"

MAGIC = b"FTZ1"

DUMPEUR = r"""
import java.time.*;
import java.time.zone.*;
import java.util.*;

/** Le tzdb du JDK, celui-la meme qu'Elasticsearch interroge. */
public class Dump {
    public static void main(String[] a) {
        List<String> ids = new ArrayList<>(ZoneId.getAvailableZoneIds());
        Collections.sort(ids);
        Map<String, Integer> jeux = new LinkedHashMap<>();
        StringBuilder zones = new StringBuilder();
        for (String id : ids) {
            String s = serialise(ZoneId.of(id).getRules());
            Integer idx = jeux.get(s);
            if (idx == null) { idx = jeux.size(); jeux.put(s, idx); }
            zones.append("Z ").append(id).append(' ').append(idx).append('\n');
        }
        System.out.println("V " + ZoneRulesProvider.getVersions("UTC").lastEntry().getKey());
        System.out.print(zones);
        for (Map.Entry<String, Integer> e : jeux.entrySet())
            System.out.print("J " + e.getValue() + "\n" + e.getKey());
    }

    static String serialise(ZoneRules r) {
        StringBuilder b = new StringBuilder();
        List<ZoneOffsetTransition> ts = r.getTransitions();
        ZoneOffset init = ts.isEmpty() ? r.getOffset(Instant.EPOCH) : ts.get(0).getOffsetBefore();
        b.append("I ").append(init.getTotalSeconds()).append('\n');
        for (ZoneOffsetTransition t : ts)
            b.append("T ").append(t.toEpochSecond())
             .append(' ').append(t.getOffsetAfter().getTotalSeconds())
             .append('\n');
        for (ZoneOffsetTransitionRule x : r.getTransitionRules())
            b.append("R ").append(x.getMonth().getValue())
             .append(' ').append(x.getDayOfMonthIndicator())
             .append(' ').append(x.getDayOfWeek() == null ? 0 : x.getDayOfWeek().getValue())
             .append(' ').append(x.getLocalTime().toSecondOfDay())
             .append(' ').append(x.isMidnightEndOfDay() ? 1 : 0)
             .append(' ').append(x.getTimeDefinition().ordinal())
             .append(' ').append(x.getStandardOffset().getTotalSeconds())
             .append(' ').append(x.getOffsetBefore().getTotalSeconds())
             .append(' ').append(x.getOffsetAfter().getTotalSeconds())
             .append('\n');
        return b.toString();
    }
}
"""


GRILLEUR = r"""
import org.elasticsearch.common.Rounding;
import org.elasticsearch.core.TimeValue;
import java.time.*;
import java.time.zone.*;
import java.util.*;

/** La grille d'arrondis, calculee par la classe Rounding d'Elasticsearch. */
public class Grille {
    /** Les intervalles poses a toutes les zones. */
    static final String[] COURANTS = {"hour", "day", "month", "3h"};
    /** Ceux poses aux zones difficiles, ou tout est exerce. */
    static final String[] TOUS = {
        "second", "minute", "hour", "day", "week", "month", "quarter", "year",
        "1ms", "15m", "90m", "3h", "12h", "1d", "7d"
    };
    /** Les zones dont les regles sortent de l'ordinaire. */
    static final String[] DIFFICILES = {
        "Europe/Paris", "America/New_York", "America/Santiago", "Australia/Lord_Howe",
        "Pacific/Chatham", "Asia/Kolkata", "Asia/Tehran", "Pacific/Apia",
        "America/Sao_Paulo", "Africa/Cairo", "Antarctica/Troll", "Asia/Kathmandu",
        "Asia/Gaza", "America/Asuncion", "UTC", "Etc/GMT+12"
    };

    public static void main(String[] a) {
        Set<String> difficiles = new HashSet<>(Arrays.asList(DIFFICILES));
        List<String> ids = new ArrayList<>(ZoneId.getAvailableZoneIds());
        Collections.sort(ids);
        StringBuilder out = new StringBuilder();
        for (String id : ids) {
            boolean dur = difficiles.contains(id);
            long[] instants = sondes(ZoneId.of(id), dur);
            for (String intervalle : (dur ? TOUS : COURANTS))
                ligne(out, id, intervalle, instants, 0);
            if (dur) {
                // `offset` : il deplace les bornes, et il se combine au fuseau.
                ligne(out, id, "day", instants, 6 * 3600_000L);
                ligne(out, id, "month", instants, -2 * 3600_000L);
            }
        }
        System.out.print(out);
    }

    static void ligne(StringBuilder out, String zone, String intervalle, long[] instants, long offset) {
        Rounding.Builder b = intervalle.matches("[0-9].*")
            ? Rounding.builder(TimeValue.parseTimeValue(intervalle, "i"))
            : Rounding.builder(unite(intervalle));
        b.timeZone(ZoneId.of(zone));
        if (offset != 0) b.offset(offset);
        Rounding r = b.build();
        Rounding.Prepared p = r.prepareForUnknown();
        Rounding.Prepared java = r.prepareJavaTime();
        out.append("{\"z\":\"").append(zone).append("\",\"i\":\"").append(intervalle)
           .append("\",\"o\":").append(offset).append(",\"c\":[");
        for (int i = 0; i < instants.length; i++) {
            long x = instants[i];
            long arrondi = p.round(x);
            long suivant = p.nextRoundingValue(arrondi);
            // Les deux chemins d'ES doivent dire la meme chose : si l'optimise
            // et le java.time divergent, la grille ne veut plus rien dire.
            if (java.round(x) != arrondi || java.nextRoundingValue(arrondi) != suivant)
                throw new IllegalStateException("les deux chemins d'ES divergent sur "
                    + zone + " " + intervalle + " " + x);
            if (i > 0) out.append(',');
            out.append('[').append(x).append(',').append(arrondi).append(',').append(suivant).append(']');
        }
        out.append("]}\n");
    }

    static Rounding.DateTimeUnit unite(String nom) {
        switch (nom) {
            case "second": return Rounding.DateTimeUnit.SECOND_OF_MINUTE;
            case "minute": return Rounding.DateTimeUnit.MINUTES_OF_HOUR;
            case "hour": return Rounding.DateTimeUnit.HOUR_OF_DAY;
            case "day": return Rounding.DateTimeUnit.DAY_OF_MONTH;
            case "week": return Rounding.DateTimeUnit.WEEK_OF_WEEKYEAR;
            case "month": return Rounding.DateTimeUnit.MONTH_OF_YEAR;
            case "quarter": return Rounding.DateTimeUnit.QUARTER_OF_YEAR;
            case "year": return Rounding.DateTimeUnit.YEAR_OF_CENTURY;
            default: throw new IllegalArgumentException(nom);
        }
    }

    /** Les instants ou l'arrondi est difficile : les bascules de la zone. */
    static long[] sondes(ZoneId zone, boolean dur) {
        ZoneRules regles = zone.getRules();
        List<Long> out = new ArrayList<>();
        for (String iso : new String[] {
            "1970-01-01T00:00:00Z", "2024-02-29T12:00:00Z", "2026-06-15T03:20:00Z",
            "2044-07-01T00:00:00Z"
        }) out.add(Instant.parse(iso).toEpochMilli());
        if (dur) {
            out.add(Instant.parse("1900-06-15T00:00:00Z").toEpochMilli());
            out.add(Instant.parse("2100-03-15T00:00:00Z").toEpochMilli());
            out.add(Instant.parse("1969-12-31T23:59:59.500Z").toEpochMilli());
        }
        Instant curseur = Instant.parse("2023-01-01T00:00:00Z");
        int voulues = dur ? 6 : 3;
        for (int i = 0; i < voulues; i++) {
            ZoneOffsetTransition t = regles.nextTransition(curseur);
            if (t == null) break;
            long e = t.getInstant().toEpochMilli();
            out.add(e - 1);
            out.add(e);
            out.add(e + 1);
            if (dur) {
                out.add(e - 1800_000L);
                out.add(e + 1800_000L);
                out.add(e - 86_400_000L);
            }
            curseur = t.getInstant().plusSeconds(1);
        }
        // Et une bascule bien apres la fin de la table, produite par les regles
        // annuelles : c'est la que le calcul remplace la lecture.
        ZoneOffsetTransition loin = regles.nextTransition(Instant.parse("2043-01-01T00:00:00Z"));
        if (loin != null) {
            out.add(loin.getInstant().toEpochMilli() - 1);
            out.add(loin.getInstant().toEpochMilli());
        }
        long[] tableau = new long[out.size()];
        for (int i = 0; i < tableau.length; i++) tableau[i] = out.get(i);
        return tableau;
    }
}
"""


# --------------------------------------------------------------------------
# Le dump, lu dans le conteneur


def conteneur():
    """Le conteneur de reference qui tourne, ou rien."""
    sortie = subprocess.run(
        ["docker", "ps", "--filter", f"ancestor={IMAGE}", "--format", "{{.Names}}"],
        capture_output=True,
        text=True,
    ).stdout.split()
    return sortie[0] if sortie else None


def dump():
    nom = conteneur()
    if not nom:
        print(f"!! aucun conteneur {IMAGE} ne tourne — la source est son JDK")
        print("   docker run -d --name es-ref -p 9201:9200 \\")
        print("     -e discovery.type=single-node -e xpack.security.enabled=false \\")
        print(f"     {IMAGE}")
        sys.exit(2)
    java = pathlib.Path("/tmp/ferrite-tz-Dump.java")
    java.write_text(DUMPEUR, encoding="utf-8")
    subprocess.run(["docker", "cp", str(java), f"{nom}:/tmp/Dump.java"], check=True)
    brut = subprocess.run(
        ["docker", "exec", nom, "sh", "-c",
         "cd /tmp && /usr/share/elasticsearch/jdk/bin/java Dump.java"],
        capture_output=True, text=True, check=True,
    ).stdout
    return lit_dump(brut)


def grille():
    """La grille d'arrondis, calculee par la classe `Rounding` d'ES."""
    nom = conteneur()
    if not nom:
        print(f"!! aucun conteneur {IMAGE} ne tourne — l'oracle est sa classe Rounding")
        sys.exit(2)
    java = pathlib.Path("/tmp/ferrite-tz-Grille.java")
    java.write_text(GRILLEUR, encoding="utf-8")
    subprocess.run(["docker", "cp", str(java), f"{nom}:/tmp/Grille.java"], check=True)
    lignes = subprocess.run(
        ["docker", "exec", nom, "sh", "-c",
         "cd /tmp && /usr/share/elasticsearch/jdk/bin/java "
         "-cp '/usr/share/elasticsearch/lib/*' Grille.java"],
        capture_output=True, text=True, check=True,
    ).stdout
    GRILLE.parent.mkdir(parents=True, exist_ok=True)
    entete = (
        '{"_": "Genere par tests/compat/genere_fuseaux.py --grille. Chaque ligne '
        'porte une zone, un intervalle, un offset et des triplets [instant, '
        'debut du seau, debut du seau suivant] — calcules par la classe '
        'org.elasticsearch.common.Rounding d\'Elasticsearch 8.15.0 elle-meme, '
        'dans le conteneur de reference. Rejoue par tests/arrondi_vs_es.rs."}\n'
    )
    GRILLE.write_text(entete + lignes, encoding="utf-8")
    cas = sum(l.count("],[") + 1 for l in lignes.splitlines())
    print(f"ok  {len(lignes.splitlines())} batteries, {cas} arrondis mesures")
    print(f"    ecrit {GRILLE.relative_to(RACINE)}")


def lit_dump(brut):
    """Le texte du dumpeur -> (version, {zone: id_jeu}, [jeu]).

    Un jeu est `(init, transitions, regles)`, ou une transition est
    `(epoch, apres)` et une regle les neuf champs de
    `ZoneOffsetTransitionRule`.
    """
    version = None
    zones = {}
    jeux = {}
    courant = None
    for ligne in brut.splitlines():
        if not ligne:
            continue
        tag, reste = ligne.split(" ", 1)
        if tag == "V":
            version = reste
        elif tag == "Z":
            nom, idx = reste.rsplit(" ", 1)
            zones[nom] = int(idx)
        elif tag == "J":
            courant = jeux.setdefault(int(reste), {"init": 0, "t": [], "r": []})
        elif tag == "I":
            courant["init"] = int(reste)
        elif tag == "T":
            courant["t"].append(tuple(int(x) for x in reste.split()))
        elif tag == "R":
            courant["r"].append(tuple(int(x) for x in reste.split()))
        else:
            raise SystemExit(f"ligne inconnue dans le dump : {ligne}")
    if version is None or not zones:
        raise SystemExit("dump vide")
    return version, zones, [jeux[i] for i in sorted(jeux)]


# --------------------------------------------------------------------------
# L'encodage : varints, pour que la table pese ce que pese le tzdb du JDK


def varint(n):
    if n < 0:
        raise ValueError(n)
    out = bytearray()
    while True:
        octet = n & 0x7F
        n >>= 7
        out.append(octet | (0x80 if n else 0))
        if not n:
            return bytes(out)


def zigzag(n):
    return varint((n << 1) ^ (n >> 63) if n < 0 else n << 1)


def encode_jeu(jeu):
    """Un jeu de regles : sa table d'offsets, ses transitions, ses regles.

    Les offsets sont mis en table et references par indice : une zone en a
    deux ou trois, et ils reviennent a chaque transition.
    """
    offsets = []

    def idx(v):
        if v not in offsets:
            offsets.append(v)
        return offsets.index(v)

    init = idx(jeu["init"])
    transitions = [(e, idx(a)) for e, a in jeu["t"]]
    regles = [
        (mois, jour, dow, sec, fin, defn, idx(std), idx(av), idx(ap))
        for mois, jour, dow, sec, fin, defn, std, av, ap in jeu["r"]
    ]

    out = bytearray()
    out += varint(len(offsets))
    for o in offsets:
        out += zigzag(o)
    out += varint(init)
    out += varint(len(transitions))
    precedent = 0
    for epoch, apres in transitions:
        out += zigzag(epoch - precedent)
        precedent = epoch
        out += varint(apres)
    out += varint(len(regles))
    for mois, jour, dow, sec, fin, defn, std, av, ap in regles:
        out += bytes([mois])
        out += zigzag(jour)
        out += bytes([dow, fin, defn])
        out += varint(sec)
        out += varint(std)
        out += varint(av)
        out += varint(ap)
    return bytes(out)


def encode(version, zones, jeux):
    corps = bytearray()
    corps += MAGIC
    corps += bytes([len(version)]) + version.encode()
    corps += varint(len(zones))
    for nom in sorted(zones):
        octets = nom.encode()
        corps += bytes([len(octets)]) + octets + varint(zones[nom])
    corps += varint(len(jeux))
    # La table des decalages : elle permet de ne decoder que le jeu dont on a
    # besoin. Sans elle, ouvrir « Europe/Paris » decoderait les 18 078
    # transitions de toutes les zones, dans un serveur dont le RSS au repos est
    # un chiffre publie.
    debut_table = len(corps)
    corps += b"\0\0\0\0" * len(jeux)
    decalages = []
    for jeu in jeux:
        decalages.append(len(corps))
        corps += encode_jeu(jeu)
    for i, d in enumerate(decalages):
        corps[debut_table + 4 * i: debut_table + 4 * i + 4] = d.to_bytes(4, "little")
    return bytes(corps)


# --------------------------------------------------------------------------
# Le decodeur, qui n'existe que pour verifier l'encodeur


def lecteur(blob):
    pos = 0

    def u8():
        nonlocal pos
        pos += 1
        return blob[pos - 1]

    def vint():
        nonlocal pos
        n = 0
        decalage = 0
        while True:
            octet = blob[pos]
            pos += 1
            n |= (octet & 0x7F) << decalage
            if not octet & 0x80:
                return n
            decalage += 7

    def zig():
        n = vint()
        return -((n + 1) >> 1) if n & 1 else n >> 1

    def octets(n):
        nonlocal pos
        pos += n
        return blob[pos - n: pos]

    def saute(n):
        nonlocal pos
        pos = n

    def ou():
        return pos

    return u8, vint, zig, octets, saute, ou


def decode(blob):
    u8, vint, zig, octets, saute, ou = lecteur(blob)
    if octets(4) != MAGIC:
        raise SystemExit("ce n'est pas une table de fuseaux")
    version = octets(u8()).decode()
    zones = {}
    for _ in range(vint()):
        nom = octets(u8()).decode()
        zones[nom] = vint()
    nb = vint()
    table = [int.from_bytes(octets(4), "little") for _ in range(nb)]
    jeux = []
    for debut in table:
        saute(debut)
        offsets = [zig() for _ in range(vint())]
        init = offsets[vint()]
        transitions = []
        epoch = 0
        for _ in range(vint()):
            epoch += zig()
            transitions.append((epoch, offsets[vint()]))
        regles = []
        for _ in range(vint()):
            mois = u8()
            jour = zig()
            dow, fin, defn = u8(), u8(), u8()
            sec = vint()
            regles.append((mois, jour, dow, sec, fin, defn,
                           offsets[vint()], offsets[vint()], offsets[vint()]))
        jeux.append({"init": init, "t": transitions, "r": regles})
    return version, zones, jeux


# --------------------------------------------------------------------------


ENTETE = '''//! La table des fuseaux horaires, **generee** — ne pas editer a la main.
//!
//! Source : le tzdb du JDK qu'embarque le conteneur Elasticsearch {image}
//! (`jdk/lib/tzdb.dat`), c'est-a-dire les regles qu'Elasticsearch lui-meme
//! applique — pas celles du systeme, que son image n'a pas.
//!
//! Version du tzdb : **{version}**. {zones} zones, {jeux} jeux de regles
//! distincts (les liens partagent les leurs), {transitions} transitions
//! historiques, {regles} regles annuelles pour le futur.
//!
//! Regenerer et verifier : `python3 tests/compat/genere_fuseaux.py [--verifie]`.
//! Le format est decrit dans ce script ; il est lu par [`crate::fuseau`].

/// La version du tzdb dont cette table est tiree.
pub const VERSION_TZDB: &str = "{version}";

/// Le nombre de zones que la table nomme.
pub const NB_ZONES: usize = {zones};

/// La table elle-meme (voir `tests/compat/genere_fuseaux.py` pour son format).
pub static TABLE: &[u8] = include_bytes!("tzdata.bin");
'''


def ecrit(version, zones, jeux, blob):
    BIN.write_bytes(blob)
    RS.write_text(
        ENTETE.format(
            image=IMAGE.split("/")[-1],
            version=version,
            zones=len(zones),
            jeux=len(jeux),
            transitions=sum(len(j["t"]) for j in jeux),
            regles=sum(len(j["r"]) for j in jeux),
        ),
        encoding="utf-8",
    )


def main():
    if "--grille" in sys.argv[1:]:
        grille()
        return
    verifie = "--verifie" in sys.argv[1:]
    version, zones, jeux = dump()
    blob = encode(version, zones, jeux)

    # Le sens retour : ce qu'on vient d'ecrire se relit-il a l'identique ?
    relu = decode(blob)
    attendu = (version, zones, jeux)
    if relu != attendu:
        for i, (a, b) in enumerate(zip(relu, attendu)):
            if a != b:
                print(f"!! la table relue differe du dump (section {i})")
        sys.exit(1)

    total = sum(len(j["t"]) for j in jeux)
    resume = (f"tzdb {version} : {len(zones)} zones, {len(jeux)} jeux de regles, "
              f"{total} transitions, {len(blob)} octets")

    if verifie:
        if not BIN.exists():
            print(f"!! {BIN} manque — lancer sans --verifie")
            sys.exit(1)
        commite = BIN.read_bytes()
        if commite != blob:
            print(f"!! {BIN} ne correspond plus au tzdb du conteneur "
                  f"({len(commite)} octets commites, {len(blob)} mesures)")
            print("   regenerer : python3 tests/compat/genere_fuseaux.py")
            sys.exit(1)
        print(f"ok  {resume} — table commitee identique au dump, dans les deux sens")
        return

    ecrit(version, zones, jeux, blob)
    print(f"ok  {resume}")
    print(f"    ecrit {BIN.relative_to(RACINE)} et {RS.relative_to(RACINE)}")


if __name__ == "__main__":
    main()
