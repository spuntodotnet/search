//! La syntaxe d'expression reguliere de Lucene, traduite vers celle du crate
//! `regex` (la seule que comprenne tantivy).
//!
//! Les deux syntaxes se ressemblent assez pour qu'on croie pouvoir passer le
//! motif tel quel — et c'est exactement le piege. Elles divergent sur des
//! caracteres courants, en silence et dans les deux sens :
//!
//! - `~`, `&`, `<1-100>`, `@`, `#` sont des **operateurs** chez Lucene et des
//!   caracteres ordinaires chez `regex` ;
//! - `^` et `$` sont des caracteres ordinaires chez Lucene (il n'a pas d'ancre :
//!   le motif est deja ancre des deux cotes) et des ancres chez `regex`, ou
//!   tantivy-fst les refuse ;
//! - `"abc"` est une chaine litterale chez Lucene, trois caracteres chez
//!   `regex` ;
//! - `\d` et ses semblables existent des deux cotes mais **pas sur le meme
//!   alphabet** : ceux de Lucene sont ASCII (`\w` ne matche pas `é`), ceux de
//!   `regex` sont Unicode ;
//! - `a*?` est un quantificateur paresseux pour `regex` — que tantivy-fst
//!   refuse — et `(a*)?` pour Lucene ;
//! - `case_insensitive` replie l'ASCII **seulement**, et seulement les
//!   caracteres isoles : `[d-e]` n'y matche pas `D`.
//!
//! Chacune de ces regles a ete **mesuree contre un vrai Elasticsearch 8.15**,
//! pas deduite de la documentation : `\d` y valait la lettre `d` jusqu'a Lucene
//! 8, et l'inverse depuis. `tests/compat/diff_motifs.py` rejoue la mesure.
//!
//! Passer le motif sans le traduire rendrait donc des documents differents de
//! ceux d'Elasticsearch sans que rien ne le signale. Ce module le traduit
//! caractere par caractere, et **refuse explicitement** les operateurs qu'un
//! automate de `regex` ne sait pas construire (complement, intersection,
//! intervalle numerique) plutot que de les prendre pour des litteraux.
//!
//! Les caracteres litteraux sont emis en `\x{..}` : c'est illisible, mais aucun
//! caractere n'y garde de sens special, dans une classe comme en dehors.

use crate::error::{EsError, EsResult};

/// Les operateurs optionnels de Lucene, actives par le parametre `flags`.
///
/// Un operateur desactive redevient un caractere litteral — c'est la seule
/// facon d'ecrire un `~` ou un `&` dans un motif.
#[derive(Clone, Copy, Debug)]
pub struct Flags {
    pub complement: bool,
    pub intersection: bool,
    pub interval: bool,
    pub anystring: bool,
    pub empty: bool,
}

impl Default for Flags {
    /// `ALL`, comme Elasticsearch quand `flags` est absent.
    fn default() -> Self {
        Self {
            complement: true,
            intersection: true,
            interval: true,
            anystring: true,
            empty: true,
        }
    }
}

impl Flags {
    pub const AUCUN: Self = Self {
        complement: false,
        intersection: false,
        interval: false,
        anystring: false,
        empty: false,
    };

    /// Lit la valeur du parametre `flags` : des noms separes par `|`.
    pub fn lire(texte: &str) -> EsResult<Self> {
        let mut f = Self::AUCUN;
        for nom in texte.split('|') {
            let nom = nom.trim().to_ascii_uppercase();
            match nom.as_str() {
                "" | "NONE" => {}
                "ALL" => f = Self::default(),
                "COMPLEMENT" => f.complement = true,
                "INTERSECTION" => f.intersection = true,
                "INTERVAL" => f.interval = true,
                "ANYSTRING" => f.anystring = true,
                "EMPTY" => f.empty = true,
                autre => {
                    return Err(EsError::illegal_argument(format!(
                        "Unknown regexp flag [{autre}]"
                    )))
                }
            }
        }
        Ok(f)
    }
}

/// Traduit un motif Lucene en motif `regex`.
///
/// `insensible` reproduit `case_insensitive` d'Elasticsearch, qui replie
/// **l'ASCII seulement** : chez Lucene comme ici, `É` ne matche pas `é`.
pub fn vers_regex(motif: &str, flags: Flags, insensible: bool) -> EsResult<String> {
    let mut p = Parseur {
        c: motif.chars().collect(),
        i: 0,
        flags,
        insensible,
    };
    let sortie = p.union()?;
    if !p.fini() {
        // Un `)` sans `(` : Lucene s'arrete la aussi.
        return Err(EsError::illegal_argument(format!(
            "[regexp] : caractere [{}] inattendu a la position {} de [{motif}]",
            p.c[p.i], p.i
        )));
    }
    // `(?s)` : chez Lucene, `.` designe n'importe quel caractere, saut de ligne
    // compris.
    Ok(format!("(?s){sortie}"))
}

/// Une chaine litterale, prete a etre concatenee dans un motif `regex`.
pub fn litteral(s: &str, insensible: bool) -> String {
    s.chars().map(|c| atome_char(c, insensible)).collect()
}

/// Un motif `wildcard` d'Elasticsearch : `*` toute suite, `?` un caractere,
/// `\` echappe le caractere suivant.
pub fn joker(s: &str, insensible: bool) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '*' => out.push_str("(?s:.*)"),
            '?' => out.push_str("(?s:.)"),
            // `\*` vaut une etoile litterale, comme chez ES. Un `\` final est
            // un `\` litteral.
            '\\' => match chars.next() {
                Some(suivant) => out.push_str(&atome_char(suivant, insensible)),
                None => out.push_str(&atome_char('\\', insensible)),
            },
            autre => out.push_str(&atome_char(autre, insensible)),
        }
    }
    out
}

// ---------------------------------------------------------------------------

struct Parseur {
    c: Vec<char>,
    i: usize,
    flags: Flags,
    insensible: bool,
}

impl Parseur {
    fn fini(&self) -> bool {
        self.i >= self.c.len()
    }

    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            return true;
        }
        false
    }

    fn suivant(&mut self) -> EsResult<char> {
        let c = self
            .peek()
            .ok_or_else(|| EsError::illegal_argument("[regexp] : motif tronque"))?;
        self.i += 1;
        Ok(c)
    }

    /// `a|b`
    fn union(&mut self) -> EsResult<String> {
        let mut out = self.intersection()?;
        while self.eat('|') {
            let droite = self.intersection()?;
            out.push('|');
            out.push_str(&droite);
        }
        Ok(out)
    }

    /// `a&b` — l'intersection de deux langages ne se construit pas avec `regex`.
    fn intersection(&mut self) -> EsResult<String> {
        let out = self.concat()?;
        if self.flags.intersection && self.peek() == Some('&') {
            return Err(refus("&", "INTERSECTION"));
        }
        Ok(out)
    }

    fn concat(&mut self) -> EsResult<String> {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            match c {
                '|' | ')' => break,
                '&' if self.flags.intersection => break,
                _ => out.push_str(&self.repetition()?),
            }
        }
        Ok(out)
    }

    /// `a?`, `a*`, `a+`, `a{2,5}` — et leurs empilements (`a*?`, legal chez
    /// Lucene, ou il ne veut pas dire « quantificateur paresseux »).
    fn repetition(&mut self) -> EsResult<String> {
        let mut atome = self.complement()?;
        let mut deja = false;
        loop {
            let quantificateur = match self.peek() {
                Some('?') => "?".to_string(),
                Some('*') => "*".to_string(),
                Some('+') => "+".to_string(),
                Some('{') => self.accolades()?,
                _ => break,
            };
            if !quantificateur.starts_with('{') {
                self.i += 1;
            }
            if deja {
                atome = format!("(?:{atome})");
            }
            atome.push_str(&quantificateur);
            deja = true;
        }
        Ok(atome)
    }

    /// `{n}`, `{n,}`, `{n,m}` — lu entierement pour refuser tot ce qui n'en est
    /// pas un, la ou `regex` en ferait des caracteres litteraux.
    fn accolades(&mut self) -> EsResult<String> {
        let depart = self.i;
        self.i += 1; // '{'
        let chiffres = |p: &mut Self| -> String {
            let mut n = String::new();
            while let Some(c) = p.peek() {
                if c.is_ascii_digit() {
                    n.push(c);
                    p.i += 1;
                } else {
                    break;
                }
            }
            n
        };
        let min = chiffres(self);
        let mut out = format!("{{{min}");
        if self.eat(',') {
            let max = chiffres(self);
            out.push(',');
            out.push_str(&max);
        }
        if min.is_empty() || !self.eat('}') {
            self.i = depart;
            return Err(EsError::illegal_argument(
                "[regexp] : repetition [{...}] malformee ; formes acceptees : {n}, {n,}, {n,m}",
            ));
        }
        out.push('}');
        Ok(out)
    }

    fn complement(&mut self) -> EsResult<String> {
        if self.flags.complement && self.peek() == Some('~') {
            return Err(refus("~", "COMPLEMENT"));
        }
        self.simple()
    }

    fn simple(&mut self) -> EsResult<String> {
        let c = self.suivant()?;
        match c {
            '.' => Ok(".".to_string()),
            '[' => self.classe(),
            '(' => {
                if self.eat(')') {
                    // Le langage reduit a la chaine vide.
                    return Ok("(?:)".to_string());
                }
                let dedans = self.union()?;
                if !self.eat(')') {
                    return Err(EsError::illegal_argument(
                        "[regexp] : parenthese ouvrante non fermee",
                    ));
                }
                Ok(format!("(?:{dedans})"))
            }
            '"' => {
                let mut litteraux = String::new();
                loop {
                    let c = self.suivant().map_err(|_| {
                        EsError::illegal_argument("[regexp] : guillemet ouvrant non ferme")
                    })?;
                    if c == '"' {
                        break;
                    }
                    litteraux.push_str(&atome_char(c, self.insensible));
                }
                Ok(format!("(?:{litteraux})"))
            }
            '@' if self.flags.anystring => Ok("(?s:.*)".to_string()),
            '#' if self.flags.empty => Err(refus("#", "EMPTY")),
            '<' if self.flags.interval => Err(refus("<n-m>", "INTERVAL")),
            '\\' => {
                let echappe = self.suivant().map_err(|_| {
                    EsError::illegal_argument("[regexp] : le motif se termine par un [\\]")
                })?;
                match predefinie(echappe) {
                    Some(plages) => Ok(emettre_classe(&plages, false)),
                    // Devant tout le reste, le backslash rend le caractere
                    // litteral — sauf devant une lettre, ou Lucene refuse.
                    None => {
                        refuser_lettre_echappee(echappe)?;
                        Ok(atome_char(echappe, self.insensible))
                    }
                }
            }
            autre => Ok(atome_char(autre, self.insensible)),
        }
    }

    /// `[abc]`, `[^a-z]`, `[a-z0-9]`, `[\d\-]`.
    fn classe(&mut self) -> EsResult<String> {
        let negative = self.eat('^');
        let mut plages: Vec<(char, char)> = Vec::new();
        loop {
            match self.peek() {
                None => {
                    return Err(EsError::illegal_argument(
                        "[regexp] : classe de caracteres non fermee",
                    ))
                }
                Some(']') => {
                    self.i += 1;
                    break;
                }
                _ => {}
            }
            match self.element_de_classe()? {
                Element::Predefinie(p) => plages.extend(p),
                Element::Char(a) => {
                    if self.eat('-') {
                        let Element::Char(b) = self.element_de_classe()? else {
                            return Err(EsError::illegal_argument(
                                "[regexp] : borne de plage invalide dans une classe",
                            ));
                        };
                        if b < a {
                            return Err(EsError::illegal_argument(format!(
                                "[regexp] : plage [{a}-{b}] inversee"
                            )));
                        }
                        // Une **plage** n'est pas repliee, meme sous
                        // `case_insensitive` : mesure faite contre ES 8.15, ou
                        // `[d-e]` ne matche pas `D`.
                        plages.push((a, b));
                    } else if self.insensible {
                        plages.extend(plier_ascii(a, a));
                    } else {
                        plages.push((a, a));
                    }
                }
            }
        }
        if plages.is_empty() {
            return Err(EsError::illegal_argument(
                "[regexp] : classe de caracteres vide",
            ));
        }
        Ok(emettre_classe(&plages, negative))
    }

    fn element_de_classe(&mut self) -> EsResult<Element> {
        let c = self.suivant()?;
        if c != '\\' {
            return Ok(Element::Char(c));
        }
        let echappe = self.suivant()?;
        match predefinie(echappe) {
            Some(plages) => Ok(Element::Predefinie(plages)),
            None => {
                refuser_lettre_echappee(echappe)?;
                Ok(Element::Char(echappe))
            }
        }
    }
}

/// Ce qu'un `[...]` contient : un caractere (qui peut ouvrir une plage) ou une
/// classe predefinie (`\d`), qui n'en ouvre pas.
enum Element {
    Char(char),
    Predefinie(Vec<(char, char)>),
}

/// Les classes predefinies de Lucene, **sur l'alphabet ASCII** : `\w` n'y
/// matche pas `é`, contrairement a celui du crate `regex`. Mesure faite contre
/// ES 8.15.
fn predefinie(c: char) -> Option<Vec<(char, char)>> {
    let chiffres = || vec![('0', '9')];
    let mots = || vec![('0', '9'), ('A', 'Z'), ('_', '_'), ('a', 'z')];
    let blancs = || vec![('\t', '\r'), (' ', ' ')];
    match c {
        'd' => Some(chiffres()),
        'D' => Some(complement(&chiffres())),
        'w' => Some(mots()),
        'W' => Some(complement(&mots())),
        's' => Some(blancs()),
        'S' => Some(complement(&blancs())),
        _ => None,
    }
}

/// Lucene refuse `\t`, `\n`, `\p{L}`, `\Q...\E` : seules les six classes
/// predefinies sont des lettres echappables. Le message reprend le sien, y
/// compris le code decimal du caractere.
fn refuser_lettre_echappee(c: char) -> EsResult<()> {
    if c.is_ascii_alphabetic() {
        return Err(EsError::illegal_argument(format!(
            "[regexp] : invalid character class \\{} ; classes predefinies acceptees : \\d, \\D, \
             \\w, \\W, \\s, \\S",
            c as u32
        )));
    }
    Ok(())
}

/// Le complementaire d'un ensemble de plages sur les scalaires Unicode.
///
/// La zone des demi-codets (`D800`-`DFFF`) est sautee : ce ne sont pas des
/// caracteres, et `regex` refuse une classe qui les contient.
fn complement(plages: &[(char, char)]) -> Vec<(char, char)> {
    const TROU: (u32, u32) = (0xD800, 0xDFFF);
    let mut points: Vec<(u32, u32)> = plages.iter().map(|(a, b)| (*a as u32, *b as u32)).collect();
    points.sort_unstable();
    let mut out = Vec::new();
    let mut curseur = 0u32;
    for (a, b) in points {
        if a > curseur {
            out.push((curseur, a - 1));
        }
        curseur = curseur.max(b + 1);
    }
    if curseur <= 0x10FFFF {
        out.push((curseur, 0x10FFFF));
    }
    out.into_iter()
        .flat_map(|(a, b)| {
            // Coupe la plage sur la zone interdite.
            let morceaux = [(a, b.min(TROU.0 - 1)), (a.max(TROU.1 + 1), b)];
            morceaux
                .into_iter()
                .filter(|(a, b)| a <= b)
                .filter_map(|(a, b)| Some((char::from_u32(a)?, char::from_u32(b)?)))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn refus(operateur: &str, flag: &str) -> EsError {
    EsError::unsupported(format!(
        "ferrite ne supporte pas l'operateur [{operateur}] de [regexp] : un automate de type \
         `regex` ne sait construire ni complement ni intersection. Desactive-le avec \
         \"flags\" (il redevient alors un caractere litteral, comme chez Elasticsearch) ; \
         [{flag}] est actif par defaut"
    ))
}

/// Un caractere litteral, sous une forme qui reste un **seul** atome (un
/// quantificateur qui suit s'applique bien a lui seul).
fn atome_char(c: char, insensible: bool) -> String {
    if insensible && c.is_ascii_alphabetic() {
        return format!(
            "[{}{}]",
            hex(c.to_ascii_lowercase()),
            hex(c.to_ascii_uppercase())
        );
    }
    hex(c)
}

fn hex(c: char) -> String {
    format!("\\x{{{:x}}}", c as u32)
}

fn emettre_classe(plages: &[(char, char)], negative: bool) -> String {
    let mut out = String::from("[");
    if negative {
        out.push('^');
    }
    for (a, b) in plages {
        out.push_str(&hex(*a));
        if b != a {
            out.push('-');
            out.push_str(&hex(*b));
        }
    }
    out.push(']');
    out
}

/// Le repliement de casse d'Elasticsearch : ASCII seulement, caractere par
/// caractere.
fn plier_ascii(a: char, b: char) -> Vec<(char, char)> {
    if a == b && a.is_ascii_alphabetic() {
        return vec![
            (a.to_ascii_lowercase(), a.to_ascii_lowercase()),
            (a.to_ascii_uppercase(), a.to_ascii_uppercase()),
        ];
    }
    vec![(a, b)]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ce que le motif traduit accepte **vraiment**, en le faisant compiler par
    /// l'automate meme que tantivy utilise derriere `RegexQuery`.
    fn accepte(motif_regex: &str, texte: &str) -> bool {
        use tantivy_fst::Automaton;
        let re = tantivy_fst::Regex::new(motif_regex).expect("regex valide");
        let mut etat = re.start();
        for octet in texte.as_bytes() {
            etat = re.accept(&etat, *octet);
        }
        re.is_match(&etat)
    }

    fn matche(motif: &str, texte: &str) -> bool {
        accepte(
            &vers_regex(motif, Flags::default(), false).expect("traduction"),
            texte,
        )
    }

    fn matche_insensible(motif: &str, texte: &str) -> bool {
        accepte(
            &vers_regex(motif, Flags::default(), true).expect("traduction"),
            texte,
        )
    }

    #[test]
    fn litteraux_et_ancrage() {
        assert!(matche("abc", "abc"));
        // Le motif est ancre des deux cotes, comme chez Lucene.
        assert!(!matche("abc", "xabcx"));
        assert!(matche(".*abc.*", "xabcx"));
    }

    #[test]
    fn classes_predefinies_ascii() {
        // Mesure contre ES 8.15 : `\d` est bien « un chiffre », et l'alphabet
        // de `\w` s'arrete a l'ASCII (la ou celui de `regex` prend tout
        // l'Unicode).
        assert!(matche("a\\dc", "a7c"));
        assert!(!matche("a\\dc", "adc"));
        assert!(matche("a\\wc", "a_c"));
        assert!(!matche("a\\wc", "a\u{e9}c"));
        assert!(matche("a\\Wc", "a\u{e9}c"));
        assert!(matche("a\\sc", "a c"));
        assert!(matche("a[\\d\\-]c", "a-c"));
        assert!(matche("a[\\d\\-]c", "a7c"));
        assert!(matche("a[\\Da]c", "a c"));
        assert!(!matche("a[^\\d]c", "a7c"));
        // Toute autre lettre echappee est refusee, comme chez Lucene.
        for motif in ["a\\tc", "a\\nc", "a\\p{L}c", "a\\Qd\\Ec"] {
            assert!(
                vers_regex(motif, Flags::default(), false).is_err(),
                "{motif} devrait etre refuse"
            );
        }
    }

    #[test]
    fn backslash_devant_un_non_lettre_rend_litteral() {
        assert!(matche("a\\.b", "a.b"));
        assert!(!matche("a\\.b", "axb"));
        assert!(matche("a\\7c", "a7c"));
        assert!(matche("a\\\\c", "a\\c"));
    }

    #[test]
    fn ancres_litterales() {
        // `^` et `$` n'ancrent rien chez Lucene : ce sont des caracteres.
        assert!(matche("^abc$", "^abc$"));
        assert!(!matche("^abc$", "abc"));
    }

    #[test]
    fn classes() {
        assert!(matche("[a-c]x", "bx"));
        assert!(!matche("[a-c]x", "dx"));
        assert!(matche("[^a-c]x", "dx"));
        assert!(matche("[-a]", "-"));
        assert!(matche("[a\\]]", "]"));
    }

    #[test]
    fn repetitions_empilees() {
        assert!(matche("ab*", "abbb"));
        assert!(matche("a{2,3}", "aaa"));
        assert!(!matche("a{2,3}", "a"));
        // `a*?` chez Lucene, c'est `(a*)?` — pas un quantificateur paresseux,
        // que tantivy-fst refuserait.
        assert!(matche("a*?", "aa"));
    }

    #[test]
    fn chaine_entre_guillemets() {
        assert!(matche("\"a*b\"", "a*b"));
        assert!(!matche("\"a*b\"", "ab"));
    }

    #[test]
    fn anystring_et_groupes() {
        assert!(matche("@", "n'importe quoi"));
        assert!(matche("(ab|cd)+", "abcd"));
        assert!(matche("()", ""));
    }

    #[test]
    fn casse_ascii_et_caracteres_isoles_seulement() {
        assert!(matche_insensible("abc", "ABC"));
        assert!(matche_insensible("\"abc\"", "ABC"));
        assert!(matche_insensible("a[d]c", "aDc"));
        assert!(matche_insensible("a[^d]c", "abc"));
        assert!(!matche_insensible("a[^d]c", "aDc"));
        // Une **plage** n'est pas repliee : mesure faite contre ES 8.15, ou
        // `a[d-e]c` ne rend pas `aDc`.
        assert!(!matche_insensible("a[d-e]c", "aDc"));
        // Le repliement ne touche pas l'accentue, la non plus.
        assert!(!matche_insensible("\u{e9}cole", "\u{c9}cole"));
    }

    #[test]
    fn operateurs_refuses_mais_desactivables() {
        for motif in ["~ab", "a&b", "<1-100>", "#"] {
            assert!(
                vers_regex(motif, Flags::default(), false).is_err(),
                "{motif} devrait etre refuse"
            );
        }
        // Desactives, ils redeviennent des caracteres litteraux.
        let flags = Flags::AUCUN;
        assert!(accepte(&vers_regex("a&b", flags, false).unwrap(), "a&b"));
        assert!(accepte(&vers_regex("~x", flags, false).unwrap(), "~x"));
        assert!(accepte(&vers_regex("#", flags, false).unwrap(), "#"));
    }

    #[test]
    fn motifs_malformes_refuses() {
        for motif in ["a(b", "a)b", "[a", "a{,2}", "a\\", "\"abc"] {
            assert!(
                vers_regex(motif, Flags::default(), false).is_err(),
                "{motif} devrait etre refuse"
            );
        }
    }

    #[test]
    fn flags_lus_comme_chez_es() {
        assert!(!Flags::lire("NONE").unwrap().complement);
        let f = Flags::lire("COMPLEMENT|INTERVAL").unwrap();
        assert!(f.complement && f.interval && !f.anystring);
        assert!(Flags::lire("ALL").unwrap().anystring);
        assert!(Flags::lire("PIRE").is_err());
    }

    #[test]
    fn joker_echappe() {
        let m = format!("(?s){}", joker("a\\*b*", false));
        assert!(accepte(&m, "a*bxx"));
        assert!(!accepte(&m, "azbxx"));
        // Insensible a la casse, sur l'ASCII seulement.
        let m = format!("(?s){}", joker("ab*", true));
        assert!(accepte(&m, "ABxx"));
    }
}
