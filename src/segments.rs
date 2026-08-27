//! Les frontieres de **phrase** et de **mot** d'UAX#29, dans le sous-ensemble
//! dont le decoupeur de fragments a besoin.
//!
//! Pourquoi ici et pas dans une bibliotheque : ce n'est pas « couper un texte
//! en phrases », c'est **couper comme Lucene coupe**. Un fragment de
//! surlignage d'Elasticsearch est delimite par le `BreakIterator` de Java, et
//! ses deux regles les moins devinables decident de tout :
//!
//! - un point suivi d'une **minuscule** n'est pas une fin de phrase (regle
//!   SB8) : « zzz cible. aaa. bbb » est **une seule** phrase, et c'est ce qui
//!   fait qu'ES rend la un fragment coupe au mot la ou une lecture naive en
//!   rendrait trois ;
//! - une apostrophe **entre deux lettres** ne coupe pas un mot (regle WB6/WB7)
//!   : `aujourd'hui` est un mot. Le tiret non plus — mais ca, ce n'est **pas**
//!   UAX#29 : c'est une jointure propre au `BreakIterator` du JDK, mesuree
//!   contre un ES 8.15 comme les autres (voir `jointures_de_java`).
//!
//! Les deux fonctions rendent une **partition** : une suite croissante de
//! frontieres qui commence a 0 et finit a la longueur du texte, en nombre de
//! `char` (l'unite d'offset de Java est l'unite UTF-16 ; la difference ne se
//! voit que sur les caracteres hors du plan multilingue de base, et elle est
//! declaree dans `docs/compat.md`).

/// Les frontieres de phrase, en indices de `char`.
pub fn phrases(chars: &[char]) -> Vec<usize> {
    let n = chars.len();
    let mut out = vec![0usize];
    if n == 0 {
        return out;
    }
    let cls: Vec<Sb> = chars.iter().map(|c| sb(*c)).collect();

    // L'etat de la regle SB9/SB10/SB11 : a-t-on vu un terminateur, puis des
    // fermantes, puis des espaces ?
    let mut terminateur: Option<Sb> = None;
    let mut apres_espace = false;
    let mut apres_sep = false;

    for i in 1..n {
        let precedent = cls[i - 1];
        let courant = cls[i];

        // SB3 : CR x LF — jamais de coupure entre les deux.
        if precedent == Sb::Cr && courant == Sb::Lf {
            continue;
        }
        // SB4 : coupure apres un separateur de paragraphe.
        if matches!(precedent, Sb::Sep | Sb::Cr | Sb::Lf) {
            out.push(i);
            terminateur = None;
            apres_espace = false;
            apres_sep = false;
            continue;
        }
        // SB5 : un Extend / Format ne change rien a l'etat.
        if courant == Sb::Extend {
            continue;
        }
        // Le caractere « significatif » precedent, Extend saute.
        let (avant, avant_avant) = contexte(&cls, i);

        // Mise a jour de l'etat du terminateur avec le caractere precedent.
        match avant {
            Some(Sb::ATerm) | Some(Sb::STerm) => {
                terminateur = avant;
                apres_espace = false;
                apres_sep = false;
            }
            Some(Sb::Close) if terminateur.is_some() && !apres_espace => {}
            Some(Sb::Sp) if terminateur.is_some() && !apres_sep => apres_espace = true,
            Some(Sb::Sep) | Some(Sb::Cr) | Some(Sb::Lf) if terminateur.is_some() => {
                apres_sep = true;
            }
            _ => {
                terminateur = None;
                apres_espace = false;
                apres_sep = false;
            }
        }

        let Some(term) = terminateur else {
            continue; // SB998
        };

        // SB6 : ATerm x Numeric.
        if term == Sb::ATerm && !apres_espace && avant == Some(Sb::ATerm) && courant == Sb::Numeric
        {
            continue;
        }
        // SB7 : (Upper|Lower) ATerm x Upper.
        if term == Sb::ATerm
            && avant == Some(Sb::ATerm)
            && courant == Sb::Upper
            && matches!(avant_avant, Some(Sb::Upper) | Some(Sb::Lower))
        {
            continue;
        }
        // SB8a : (STerm|ATerm) Close* Sp* x (SContinue|STerm|ATerm).
        if matches!(courant, Sb::SContinue | Sb::STerm | Sb::ATerm) && !apres_sep {
            continue;
        }
        // SB9 : (STerm|ATerm) Close* x (Close|Sp|Sep|CR|LF).
        // SB10 : (STerm|ATerm) Close* Sp* x (Sp|Sep|CR|LF).
        if matches!(courant, Sb::Sp | Sb::Sep | Sb::Cr | Sb::Lf) && !apres_sep {
            continue;
        }
        if courant == Sb::Close && !apres_espace {
            continue;
        }
        // SB8 : ATerm Close* Sp* x (non-frontiere)* Lower.
        if term == Sb::ATerm && !apres_sep && suivi_de_minuscule(&cls, i) {
            continue;
        }
        // SB11 : partout ailleurs apres un terminateur, on coupe.
        out.push(i);
        terminateur = None;
        apres_espace = false;
        apres_sep = false;
    }
    if *out.last().expect("au moins 0") != n {
        out.push(n);
    }
    out
}

/// Le caractere significatif precedant `i`, et celui d'avant — les Extend et
/// Format sautes (regle SB5).
fn contexte(cls: &[Sb], i: usize) -> (Option<Sb>, Option<Sb>) {
    let mut j = i;
    let mut vus = [None, None];
    let mut k = 0;
    while j > 0 && k < 2 {
        j -= 1;
        if cls[j] == Sb::Extend {
            continue;
        }
        vus[k] = Some(cls[j]);
        k += 1;
    }
    (vus[0], vus[1])
}

/// SB8 : depuis `i`, sauter ce qui n'est ni lettre capitale, ni terminateur, ni
/// fermante, ni espace, ni separateur — et regarder si on tombe sur une
/// minuscule. C'est la regle qui fait que « ... cible. aaa ... » ne coupe pas.
fn suivi_de_minuscule(cls: &[Sb], i: usize) -> bool {
    for c in &cls[i..] {
        match c {
            Sb::Lower => return true,
            Sb::OLetter
            | Sb::Upper
            | Sb::STerm
            | Sb::ATerm
            | Sb::Close
            | Sb::Sp
            | Sb::Sep
            | Sb::Cr
            | Sb::Lf => return false,
            _ => {}
        }
    }
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sb {
    Cr,
    Lf,
    Sep,
    Sp,
    Lower,
    Upper,
    OLetter,
    Numeric,
    ATerm,
    STerm,
    Close,
    SContinue,
    Extend,
    Autre,
}

fn sb(c: char) -> Sb {
    match c {
        '\r' => return Sb::Cr,
        '\n' => return Sb::Lf,
        '\u{0085}' | '\u{2028}' | '\u{2029}' => return Sb::Sep,
        '.' | '\u{2024}' | '\u{FE52}' | '\u{FF0E}' => return Sb::ATerm,
        '!'
        | '?'
        | '\u{0589}'
        | '\u{061F}'
        | '\u{06D4}'
        | '\u{0700}'..='\u{0702}'
        | '\u{07F9}'
        | '\u{0964}'
        | '\u{0965}'
        | '\u{104A}'
        | '\u{104B}'
        | '\u{1362}'
        | '\u{1367}'
        | '\u{1368}'
        | '\u{166E}'
        | '\u{1803}'
        | '\u{1809}'
        | '\u{203C}'
        | '\u{203D}'
        | '\u{2047}'..='\u{2049}'
        | '\u{3002}'
        | '\u{FE56}'
        | '\u{FE57}'
        | '\u{FF01}'
        | '\u{FF1F}'
        | '\u{FF61}' => return Sb::STerm,
        ',' | '-' | ':' | '\u{055D}' | '\u{060C}' | '\u{060D}' | '\u{07F8}' | '\u{1802}'
        | '\u{1808}' | '\u{2013}' | '\u{2014}' | '\u{3001}' | '\u{FE10}' | '\u{FE11}'
        | '\u{FE13}' | '\u{FE31}' | '\u{FE32}' | '\u{FE50}' | '\u{FE51}' | '\u{FE55}'
        | '\u{FE58}' | '\u{FE63}' | '\u{FF0C}' | '\u{FF0D}' | '\u{FF1A}' | '\u{FF64}' => {
            return Sb::SContinue
        }
        '\'' | '"' | '\u{2018}' | '\u{2019}' | '\u{201C}' | '\u{201D}' => return Sb::Close,
        _ => {}
    }
    if est_extend(c) {
        return Sb::Extend;
    }
    if est_fermante(c) {
        return Sb::Close;
    }
    if c.is_whitespace() {
        return Sb::Sp;
    }
    if crate::ngram::est_chiffre(c) {
        return Sb::Numeric;
    }
    if crate::ngram::est_lettre(c) {
        // `Lower` et `Upper` sont les proprietes Unicode, pas les categories :
        // c'est ce que fait UAX#29, et la difference se voit sur les capitales
        // de titre.
        if c.is_lowercase() {
            return Sb::Lower;
        }
        if c.is_uppercase() {
            return Sb::Upper;
        }
        return Sb::OLetter;
    }
    Sb::Autre
}

/// Les `Extend` et `Format` d'UAX#29, dans ce qui se rencontre : les marques
/// combinantes des ecritures latines et indiennes, les selecteurs de variante
/// et les caracteres de mise en forme invisibles.
///
/// Les tables generees de ce depot ne portent que Lu..Lo, Nd, P* et S* : cette
/// liste est donc ecrite ici, et sa portee est declaree dans `docs/compat.md`.
fn est_extend(c: char) -> bool {
    matches!(c as u32,
        0x00AD | 0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD | 0x05BF
        | 0x05C1..=0x05C2 | 0x0610..=0x061A | 0x064B..=0x065F | 0x0670
        | 0x06D6..=0x06DC | 0x06DF..=0x06E4 | 0x0730..=0x074A | 0x07A6..=0x07B0
        | 0x0900..=0x0903 | 0x093A..=0x094F | 0x0951..=0x0957 | 0x0962..=0x0963
        | 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E | 0x0F71..=0x0F84
        | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x200B..=0x200F | 0x202A..=0x202E
        | 0x2060..=0x2064 | 0x20D0..=0x20F0 | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F
        | 0xFEFF | 0xFFF9..=0xFFFB | 0xE0100..=0xE01EF)
}

/// Les `Close` d'UAX#29 : parentheses, crochets, guillemets — ce qui peut
/// s'intercaler entre un point et la fin de la phrase (`SB9`).
fn est_fermante(c: char) -> bool {
    matches!(
        c,
        '(' | ')'
            | '['
            | ']'
            | '{'
            | '}'
            | '«'
            | '»'
            | '\u{2039}'
            | '\u{203A}'
            | '\u{300C}'
            | '\u{300D}'
            | '\u{FF08}'
            | '\u{FF09}'
    )
}

// ---------------------------------------------------------------------------
// Mots
// ---------------------------------------------------------------------------

/// Les frontieres de mot, en indices de `char`.
pub fn mots(chars: &[char]) -> Vec<usize> {
    let n = chars.len();
    let mut out = vec![0usize];
    if n == 0 {
        return out;
    }
    let cls: Vec<Wb> = chars.iter().map(|c| wb(*c)).collect();
    for i in 1..n {
        if coupe_mot(&cls, i) {
            out.push(i);
        }
    }
    if *out.last().expect("au moins 0") != n {
        out.push(n);
    }
    out
}

fn coupe_mot(cls: &[Wb], i: usize) -> bool {
    let prec = cls[i - 1];
    let cour = cls[i];
    // WB3 / WB3a / WB3b : les fins de ligne coupent, sauf CR LF.
    if prec == Wb::Cr && cour == Wb::Lf {
        return false;
    }
    if matches!(prec, Wb::Cr | Wb::Lf | Wb::Newline)
        || matches!(cour, Wb::Cr | Wb::Lf | Wb::Newline)
    {
        return true;
    }
    // WB3d : les espaces d'un meme souffle restent ensemble.
    if prec == Wb::WSegSpace && cour == Wb::WSegSpace {
        return false;
    }
    // WB4 : Extend / Format ne coupent pas et ne comptent pas.
    if matches!(cour, Wb::Extend) {
        return false;
    }
    let avant = precedent_significatif(cls, i);
    let Some(avant) = avant else {
        return true;
    };
    let apres = suivant_significatif(cls, i + 1);
    let avant_avant = avant_du_precedent(cls, i);

    let lettre = |w: Wb| w == Wb::ALetter;
    let num = |w: Wb| w == Wb::Numeric;

    // WB5 / WB8 / WB9 / WB10.
    if (lettre(avant) || num(avant)) && (lettre(cour) || num(cour)) {
        return false;
    }
    // WB6 : ALetter x (MidLetter|MidNumLet|SingleQuote) ALetter.
    if lettre(avant) && cour.joint_lettre() && apres.is_some_and(lettre) {
        return false;
    }
    // WB7 : ALetter (MidLetter|MidNumLet|SingleQuote) x ALetter.
    if lettre(cour) && avant.joint_lettre() && avant_avant.is_some_and(lettre) {
        return false;
    }
    // WB11 : Numeric (MidNum|MidNumLet|SingleQuote) x Numeric.
    if num(cour) && avant.joint_nombre() && avant_avant.is_some_and(num) {
        return false;
    }
    // WB12 : Numeric x (MidNum|MidNumLet|SingleQuote) Numeric.
    if num(avant) && cour.joint_nombre() && apres.is_some_and(num) {
        return false;
    }
    // WB13a / WB13b : le connecteur `_`.
    if cour == Wb::ExtendNumLet && (lettre(avant) || num(avant) || avant == Wb::ExtendNumLet) {
        return false;
    }
    if avant == Wb::ExtendNumLet && (lettre(cour) || num(cour)) {
        return false;
    }
    // WB999.
    true
}

fn precedent_significatif(cls: &[Wb], i: usize) -> Option<Wb> {
    (0..i).rev().map(|j| cls[j]).find(|w| *w != Wb::Extend)
}

fn avant_du_precedent(cls: &[Wb], i: usize) -> Option<Wb> {
    let mut vus = 0;
    for j in (0..i).rev() {
        if cls[j] == Wb::Extend {
            continue;
        }
        vus += 1;
        if vus == 2 {
            return Some(cls[j]);
        }
    }
    None
}

fn suivant_significatif(cls: &[Wb], i: usize) -> Option<Wb> {
    cls.get(i..)?.iter().copied().find(|w| *w != Wb::Extend)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wb {
    Cr,
    Lf,
    Newline,
    WSegSpace,
    ALetter,
    Numeric,
    ExtendNumLet,
    MidLetter,
    MidNumLet,
    MidNum,
    SingleQuote,
    DoubleQuote,
    Extend,
    Autre,
}

impl Wb {
    /// Ce qui joint deux lettres (regles WB6 / WB7, plus les jointures propres
    /// a Java).
    fn joint_lettre(self) -> bool {
        matches!(
            self,
            Self::MidLetter | Self::MidNumLet | Self::SingleQuote | Self::DoubleQuote
        )
    }

    /// Ce qui joint deux chiffres (regles WB11 / WB12).
    fn joint_nombre(self) -> bool {
        matches!(self, Self::MidNum | Self::MidNumLet | Self::SingleQuote)
    }
}

fn wb(c: char) -> Wb {
    match c {
        '\r' => return Wb::Cr,
        '\n' => return Wb::Lf,
        '\u{000B}' | '\u{000C}' | '\u{0085}' | '\u{2028}' | '\u{2029}' => return Wb::Newline,
        // Les jointures **de Java**, pas celles d'UAX#29 : c'est le
        // `BreakIterator` du JDK qui decoupe les fragments d'ES, et ses
        // divergences se voient sur des caracteres courants. Mesurees une par
        // une contre un ES 8.15 (`no_match_size: 1` dit ou tombe la premiere
        // frontiere) :
        //
        //   `abcde-fghij`  un mot     — le tiret joint deux lettres
        //   `abcde"fghij`  un mot     — le guillemet droit aussi
        //   `abcde:fghij`  deux mots  — le deux-points **non**, la ou UAX#29
        //                               en fait un MidLetter
        //   `abcde’fghij`  deux mots  — l'apostrophe typographique non plus
        //   `abcde·fghij`  deux mots
        //
        // Le tiret est celui qui compte : sans lui, `tiret-bas` se coupait en
        // « tiret » (trouve par le fuzzer, graine 31).
        '\'' => return Wb::SingleQuote,
        '"' => return Wb::DoubleQuote,
        '-' | '\u{2010}'..='\u{2015}' | '\u{05F4}' | '\u{2027}' | '\u{FE13}' | '\u{FE55}' => {
            return Wb::MidLetter
        }
        '.' | '\u{2024}' | '\u{FE52}' | '\u{FF0E}' => return Wb::MidNumLet,
        ',' | ';' | '\u{037E}' | '\u{0589}' | '\u{060C}' | '\u{060D}' | '\u{066C}' | '\u{07F8}'
        | '\u{2044}' | '\u{FE10}' | '\u{FE14}' | '\u{FE50}' | '\u{FE54}' | '\u{FF0C}'
        | '\u{FF1B}' => return Wb::MidNum,
        '_' => return Wb::ExtendNumLet,
        _ => {}
    }
    if est_extend(c) {
        return Wb::Extend;
    }
    // Pc — les connecteurs, dont `_` traite plus haut.
    if matches!(
        c as u32,
        0x203F | 0x2040 | 0x2054 | 0xFE33 | 0xFE34 | 0xFE4D..=0xFE4F | 0xFF3F
    ) {
        return Wb::ExtendNumLet;
    }
    // Zs — l'espace « de segmentation » d'UAX#29 (regle WB3d) : les fins de
    // ligne, elles, sont deja parties plus haut.
    if c.is_whitespace() {
        return Wb::WSegSpace;
    }
    if crate::ngram::est_chiffre(c) {
        return Wb::Numeric;
    }
    if crate::ngram::est_lettre(c) {
        return Wb::ALetter;
    }
    Wb::Autre
}

// ---------------------------------------------------------------------------
// Acces « facon BreakIterator »
// ---------------------------------------------------------------------------

/// La derniere frontiere **strictement** avant `offset` — le `preceding` de
/// Java.
pub fn precedente(bornes: &[usize], offset: usize) -> usize {
    match bornes.binary_search(&offset) {
        Ok(0) => 0,
        Ok(i) => bornes[i - 1],
        Err(0) => 0,
        Err(i) => bornes[i - 1],
    }
}

/// La premiere frontiere **strictement** apres `offset` — le `following` de
/// Java. Rend la fin du texte quand il n'y en a plus.
pub fn suivante(bornes: &[usize], offset: usize) -> usize {
    match bornes.binary_search(&offset) {
        Ok(i) if i + 1 < bornes.len() => bornes[i + 1],
        Ok(_) => *bornes.last().expect("au moins 0"),
        Err(i) if i < bornes.len() => bornes[i],
        Err(_) => *bornes.last().expect("au moins 0"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoupe(texte: &str, bornes: Vec<usize>) -> Vec<String> {
        let cs: Vec<char> = texte.chars().collect();
        bornes
            .windows(2)
            .map(|w| cs[w[0]..w[1]].iter().collect())
            .collect()
    }

    fn ph(texte: &str) -> Vec<String> {
        let cs: Vec<char> = texte.chars().collect();
        decoupe(texte, phrases(&cs))
    }

    fn mo(texte: &str) -> Vec<String> {
        let cs: Vec<char> = texte.chars().collect();
        decoupe(texte, mots(&cs))
    }

    /// La regle qui decide de tout : un point suivi d'une **minuscule** ne
    /// termine pas une phrase. Mesure contre un ES 8.15 (`diff_highlight.py`).
    #[test]
    fn point_suivi_de_minuscule_ne_coupe_pas() {
        assert_eq!(ph("zzz cible. aaa. bbb"), ["zzz cible. aaa. bbb"]);
        assert_eq!(ph("Aa. Bb."), ["Aa. ", "Bb."]);
    }

    #[test]
    fn phrases_ordinaires() {
        assert_eq!(
            ph("Le chat dort. Le chien aboie."),
            ["Le chat dort. ", "Le chien aboie."]
        );
        assert_eq!(
            ph("Bonjour ? Tres bien ! Et vous."),
            ["Bonjour ? ", "Tres bien ! ", "Et vous."]
        );
    }

    /// L'espace de fin appartient a la phrase qui precede : c'est ce qui rend
    /// la longueur d'une fusion de phrases comparable a `fragment_size`.
    #[test]
    fn l_espace_final_reste_dans_la_phrase() {
        let cs: Vec<char> = "Aa. Bb.".chars().collect();
        assert_eq!(phrases(&cs), [0, 4, 7]);
    }

    /// SB6 et SB7 : un point entre chiffres ou entre capitales ne coupe pas.
    #[test]
    fn abreviations_et_nombres() {
        assert_eq!(ph("version 8.15 ok"), ["version 8.15 ok"]);
        assert_eq!(ph("U.S.A. Ensuite"), ["U.S.A. ", "Ensuite"]);
    }

    #[test]
    fn mots_avec_apostrophe() {
        assert_eq!(mo("aujourd'hui"), ["aujourd'hui"]);
        // Le tiret joint, chez Java : voir `jointures_de_java`.
        assert_eq!(mo("allez-vous"), ["allez-vous"]);
        assert_eq!(mo("a b"), ["a", " ", "b"]);
        assert_eq!(mo("a   b"), ["a", "   ", "b"]);
        assert_eq!(mo("1,5 kg"), ["1,5", " ", "kg"]);
    }

    /// Les jointures **de Java**, mesurees contre un ES 8.15 : ce sont elles
    /// qui coupent les fragments, pas celles d'UAX#29.
    #[test]
    fn jointures_de_java() {
        assert_eq!(mo("tiret-bas"), ["tiret-bas"]);
        assert_eq!(mo("abcde-fghij-klmno"), ["abcde-fghij-klmno"]);
        assert_eq!(mo("abcde--fghij"), ["abcde", "-", "-", "fghij"]);
        assert_eq!(mo("abcde.fghij"), ["abcde.fghij"]);
        assert_eq!(mo("abcde\"fghij"), ["abcde\"fghij"]);
        // Le deux-points est un MidLetter chez UAX#29, pas chez Java.
        assert_eq!(mo("abcde:fghij"), ["abcde", ":", "fghij"]);
        assert_eq!(mo("abcde\u{2019}fghij"), ["abcde", "\u{2019}", "fghij"]);
        // Un tiret ne joint pas deux nombres, ni une lettre et un chiffre.
        assert_eq!(mo("12345-67890"), ["12345", "-", "67890"]);
        assert_eq!(mo("abcde-12345"), ["abcde", "-", "12345"]);
        assert_eq!(mo("12345,67890"), ["12345,67890"]);
    }

    #[test]
    fn preceding_et_following() {
        let b = vec![0, 10, 11, 15, 16, 21, 22, 26];
        assert_eq!(precedente(&b, 15), 11);
        assert_eq!(precedente(&b, 16), 15);
        assert_eq!(precedente(&b, 0), 0);
        assert_eq!(suivante(&b, 10), 11);
        assert_eq!(suivante(&b, 12), 15);
        assert_eq!(suivante(&b, 26), 26);
    }
}
