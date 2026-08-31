//! Les stemmers de Lucene, portes en Rust.
//!
//! # Pourquoi ne pas prendre celui de tantivy
//!
//! tantivy expose un stemmer Snowball. Elasticsearch n'utilise pas Snowball
//! pour ses analyzers de langue : `english` s'appuie sur le **Porter** original
//! et `french` sur le **stemmer leger** de Jacques Savoy. Les trois algorithmes
//! rendent des termes differents — mesure sur 28 textes avant ce module : 19
//! divergences en `english`, 17 en `french`. Porter le nom d'ES en indexant
//! autre chose changerait silencieusement les resultats d'un mapping existant,
//! ce que ce projet refuse.
//!
//! Ces deux algorithmes sont donc reecrits ici, et `tests/compat/diff_analyzers.py`
//! les confronte token par token a ceux d'un vrai Elasticsearch.

/// Le stemmer Porter original, celui de `PorterStemFilter` chez Lucene.
///
/// L'algorithme travaille sur la forme *consonne/voyelle* du mot : `m` compte
/// les alternances VC apres le prefixe initial, et c'est lui qui decide si un
/// suffixe peut tomber.
pub fn porter(mot: &str) -> String {
    // Lucene n'ecarte pas les tokens non-ASCII : tout ce qui n'est pas une
    // voyelle compte comme une consonne, apostrophes et accents compris. C'est
    // ce qui donne `l'ascension` -> `l'ascens` et `coûte` -> `coût`.
    if mot.chars().count() <= 2 {
        return mot.to_string();
    }
    let mut b: Vec<char> = mot.chars().collect();
    etape1a(&mut b);
    etape1b(&mut b);
    etape1c(&mut b);
    etape2(&mut b);
    etape3(&mut b);
    etape4(&mut b);
    etape5(&mut b);
    b.into_iter().collect()
}

fn est_consonne(b: &[char], i: usize) -> bool {
    match b[i] {
        'a' | 'e' | 'i' | 'o' | 'u' => false,
        'y' => i == 0 || !est_consonne(b, i - 1),
        _ => true,
    }
}

/// Le `m` de Porter : le nombre de sequences voyelle-consonne.
fn mesure(b: &[char]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < b.len() && est_consonne(b, i) {
        i += 1;
    }
    loop {
        while i < b.len() && !est_consonne(b, i) {
            i += 1;
        }
        if i >= b.len() {
            return n;
        }
        n += 1;
        while i < b.len() && est_consonne(b, i) {
            i += 1;
        }
        if i >= b.len() {
            return n;
        }
    }
}

/// La racine contient-elle une voyelle ?
fn a_une_voyelle(b: &[char]) -> bool {
    (0..b.len()).any(|i| !est_consonne(b, i))
}

/// Se termine par une double consonne ?
fn double_consonne(b: &[char]) -> bool {
    b.len() >= 2 && b[b.len() - 1] == b[b.len() - 2] && est_consonne(b, b.len() - 1)
}

/// Consonne-voyelle-consonne, la derniere n'etant ni w, ni x, ni y.
fn cvc(b: &[char]) -> bool {
    let n = b.len();
    if n < 3 {
        return false;
    }
    est_consonne(b, n - 3)
        && !est_consonne(b, n - 2)
        && est_consonne(b, n - 1)
        && !matches!(b[n - 1], 'w' | 'x' | 'y')
}

fn finit_par(b: &[char], suffixe: &str) -> bool {
    let s: Vec<char> = suffixe.chars().collect();
    b.len() >= s.len() && b[b.len() - s.len()..] == s[..]
}

/// Remplace `suffixe` par `par` si la mesure de la racine passe le seuil.
fn remplace(b: &mut Vec<char>, suffixe: &str, par: &str, seuil: usize) -> bool {
    if !finit_par(b, suffixe) {
        return false;
    }
    let racine = b.len() - suffixe.chars().count();
    if mesure(&b[..racine]) > seuil {
        b.truncate(racine);
        b.extend(par.chars());
        true
    } else {
        false
    }
}

fn etape1a(b: &mut Vec<char>) {
    if finit_par(b, "sses") || finit_par(b, "ies") {
        b.truncate(b.len() - 2);
    } else if b.last() == Some(&'s') && !finit_par(b, "ss") {
        b.pop();
    }
}

fn etape1b(b: &mut Vec<char>) {
    let mut poursuivre = false;
    if finit_par(b, "eed") {
        if mesure(&b[..b.len() - 3]) > 0 {
            b.pop();
        }
    } else if finit_par(b, "ed") && a_une_voyelle(&b[..b.len() - 2]) {
        b.truncate(b.len() - 2);
        poursuivre = true;
    } else if finit_par(b, "ing") && a_une_voyelle(&b[..b.len() - 3]) {
        b.truncate(b.len() - 3);
        poursuivre = true;
    }
    if !poursuivre {
        return;
    }
    if finit_par(b, "at") || finit_par(b, "bl") || finit_par(b, "iz") {
        b.push('e');
    } else if double_consonne(b) && !matches!(b[b.len() - 1], 'l' | 's' | 'z') {
        b.pop();
    } else if mesure(b) == 1 && cvc(b) {
        b.push('e');
    }
}

fn etape1c(b: &mut [char]) {
    let n = b.len();
    if n > 1 && b[n - 1] == 'y' && a_une_voyelle(&b[..n - 1]) {
        b[n - 1] = 'i';
    }
}

fn etape2(b: &mut Vec<char>) {
    const REGLES: &[(&str, &str)] = &[
        ("ational", "ate"),
        ("tional", "tion"),
        ("enci", "ence"),
        ("anci", "ance"),
        ("izer", "ize"),
        ("bli", "ble"),
        ("alli", "al"),
        ("entli", "ent"),
        ("eli", "e"),
        ("ousli", "ous"),
        ("ization", "ize"),
        ("ation", "ate"),
        ("ator", "ate"),
        ("alism", "al"),
        ("iveness", "ive"),
        ("fulness", "ful"),
        ("ousness", "ous"),
        ("aliti", "al"),
        ("iviti", "ive"),
        ("biliti", "ble"),
        ("logi", "log"),
    ];
    for (de, vers) in REGLES {
        if remplace(b, de, vers, 0) {
            return;
        }
    }
}

fn etape3(b: &mut Vec<char>) {
    const REGLES: &[(&str, &str)] = &[
        ("icate", "ic"),
        ("ative", ""),
        ("alize", "al"),
        ("iciti", "ic"),
        ("ical", "ic"),
        ("ful", ""),
        ("ness", ""),
    ];
    for (de, vers) in REGLES {
        if remplace(b, de, vers, 0) {
            return;
        }
    }
}

fn etape4(b: &mut Vec<char>) {
    const SUFFIXES: &[&str] = &[
        "al", "ance", "ence", "er", "ic", "able", "ible", "ant", "ement", "ment", "ent", "ou",
        "ism", "ate", "iti", "ous", "ive", "ize",
    ];
    // `ion` ne tombe qu'apres un `s` ou un `t`.
    if finit_par(b, "ion") {
        let racine = b.len() - 3;
        if racine > 0 && matches!(b[racine - 1], 's' | 't') && mesure(&b[..racine]) > 1 {
            b.truncate(racine);
            return;
        }
    }
    // Les plus longs d'abord, sinon `ement` serait mange par `ment`.
    let mut ordonnes: Vec<&&str> = SUFFIXES.iter().collect();
    ordonnes.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for s in ordonnes {
        if finit_par(b, s) {
            let racine = b.len() - s.chars().count();
            if mesure(&b[..racine]) > 1 {
                b.truncate(racine);
            }
            return;
        }
    }
}

fn etape5(b: &mut Vec<char>) {
    if b.last() == Some(&'e') {
        let racine = &b[..b.len() - 1];
        let m = mesure(racine);
        if m > 1 || (m == 1 && !cvc(racine)) {
            b.pop();
        }
    }
    if b.len() > 1 && b[b.len() - 1] == 'l' && double_consonne(b) && mesure(b) > 1 {
        b.pop();
    }
}

/// Le stemmer leger francais de Lucene (`FrenchLightStemFilter`, d'apres
/// Jacques Savoy).
///
/// Beaucoup plus court que Porter, et volontairement prudent : il coupe les
/// marques de nombre et de genre, quelques suffixes derivationnels frequents,
/// puis normalise les doubles consonnes et les accents restants.
pub fn french_light(mot: &str) -> String {
    let mut s: Vec<char> = mot.chars().collect();
    let mut n = s.len();

    if n > 5 && s[n - 1] == 'x' {
        // « chevaux » -> « cheval », mais pas « eaux ».
        if s[n - 3] == 'a' && s[n - 2] == 'u' && s[n - 4] != 'e' {
            s[n - 2] = 'l';
        }
        n -= 1;
    }
    if n > 3 && matches!(s[n - 1], 'x' | 's') {
        n -= 1;
    }

    macro_rules! fin {
        ($suffixe:expr) => {
            n >= $suffixe.chars().count()
                && s[n - $suffixe.chars().count()..n]
                    .iter()
                    .collect::<String>()
                    == $suffixe
        };
    }

    if n > 9 && fin!("issement") {
        n -= 6;
        s[n - 1] = 'i';
        return normalise(&s[..n]);
    }
    if n > 8 && fin!("issant") {
        n -= 4;
        s[n - 1] = 'i';
        return normalise(&s[..n]);
    }
    if n > 6 && fin!("ement") {
        n -= 4;
        if n > 3 && fin!("ive") {
            n -= 1;
            s[n - 1] = 'f';
        }
        return normalise(&s[..n]);
    }
    // « relative » -> « relatif », mais « naive » reste « naiv » : la regle ne
    // s applique qu au-dela de 5 caracteres.
    if n > 5 && fin!("ive") {
        n -= 1;
        s[n - 1] = 'f';
        return normalise(&s[..n]);
    }
    if n > 11 && fin!("ficatrice") {
        n -= 5;
        s[n - 2] = 'e';
        s[n - 1] = 'r';
        return normalise(&s[..n]);
    }
    if n > 10 && fin!("ficateur") {
        n -= 4;
        s[n - 2] = 'e';
        s[n - 1] = 'r';
        return normalise(&s[..n]);
    }
    // « multiplicatrice » -> « multipliqu » : le radical reprend le `qu`.
    if n > 9 && fin!("catrice") {
        n -= 5;
        s[n - 2] = 'q';
        s[n - 1] = 'u';
        return normalise(&s[..n]);
    }
    if n > 8 && fin!("cateur") {
        n -= 4;
        s[n - 2] = 'q';
        s[n - 1] = 'u';
        return normalise(&s[..n]);
    }
    if n > 8 && fin!("atrice") {
        n -= 4;
        s[n - 2] = 'e';
        s[n - 1] = 'r';
        return normalise(&s[..n]);
    }
    if n > 7 && fin!("ateur") {
        n -= 3;
        s[n - 2] = 'e';
        s[n - 1] = 'r';
        return normalise(&s[..n]);
    }
    if n > 6 && fin!("trice") {
        n -= 1;
        s[n - 3] = 'e';
        s[n - 2] = 'u';
        s[n - 1] = 'r';
    }
    // « organization » -> « organiz », « condition » -> « cond » (le `e` pose
    // ici devient « ...ie », que la normalisation coupe).
    if n > 8 && fin!("ation") {
        return normalise(&s[..n - 5]);
    }
    if n > 8 && fin!("ition") {
        n -= 3;
        s[n - 1] = 'e';
        return normalise(&s[..n]);
    }
    if n > 5 && fin!("ième") {
        return normalise(&s[..n - 4]);
    }
    if n > 7 && fin!("teuse") {
        n -= 2;
        s[n - 1] = 'r';
        return normalise(&s[..n]);
    }
    if n > 6 && fin!("teur") {
        n -= 1;
        s[n - 1] = 'r';
        return normalise(&s[..n]);
    }
    if n > 5 && fin!("euse") {
        return normalise(&s[..n - 2]);
    }
    if n > 8 && fin!("ère") {
        n -= 1;
        s[n - 2] = 'e';
        return normalise(&s[..n]);
    }
    if n > 7 && fin!("ese") {
        return normalise(&s[..n - 3]);
    }
    if n > 6 && fin!("welt") {
        return normalise(&s[..n]);
    }
    if n > 5 && fin!("aux") {
        s[n - 2] = 'l';
        return normalise(&s[..n - 1]);
    }
    normalise(&s[..n])
}

/// La normalisation finale de Savoy.
///
/// Elle fait le gros du travail : accents rabattus, **lettres doublees
/// supprimees partout dans le mot** (« arriviste » -> « ariviste », « homme »
/// -> « home »), puis quelques coupes finales. C'est elle qui explique la
/// plupart des formes rendues par ES.
fn normalise(s: &[char]) -> String {
    let mut out: Vec<char> = s.to_vec();
    if out.len() > 4 {
        for c in out.iter_mut() {
            *c = match *c {
                'à' | 'á' | 'â' => 'a',
                'ô' => 'o',
                // Savoy replie `î` mais **pas** `ï`, ni `ë` : mesure sur
                // « naïve » et « noël », que ES rend `naïv` et `noël`.
                'è' | 'é' | 'ê' => 'e',
                'ù' | 'û' => 'u',
                'î' => 'i',
                'ç' => 'c',
                autre => autre,
            };
        }
        // Une lettre identique a la precedente disparait, ou qu'elle soit.
        let mut precedente = out[0];
        let mut i = 1;
        while i < out.len() {
            if out[i] == precedente && precedente.is_alphabetic() {
                out.remove(i);
            } else {
                precedente = out[i];
                i += 1;
            }
        }
    }
    if out.len() > 4 && out[out.len() - 2..] == ['i', 'e'] {
        out.truncate(out.len() - 2);
    }
    if out.len() > 4 {
        if out[out.len() - 1] == 'r' {
            out.pop();
        }
        if out[out.len() - 1] == 'e' {
            out.pop();
        }
        if out[out.len() - 1] == 'e' {
            out.pop();
        }
        if out[out.len() - 1] == out[out.len() - 2] && out[out.len() - 1].is_alphabetic() {
            out.pop();
        }
    }
    out.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Les stemmers legers de Savoy, pour les quatre langues ou ES n'emploie pas
// Snowball
// ---------------------------------------------------------------------------
//
// Les analyzers `german`, `spanish`, `italian` et `portuguese` d'Elasticsearch
// posent un stemmer **leger** (`light_german`, `light_spanish`...), pas
// l'algorithme Snowball du meme nom — mesure : sur les 35 053 mots du
// vocabulaire allemand du projet Snowball, la chaine batie avec Snowball
// s'ecarte de l'analyzer `german` sur 445 mots, celle batie avec le stemmer
// leger sur 0.
//
// C'est la meme famille que `french_light` ci-dessus (Jacques Savoy), et ils se
// portent comme lui : quelques dizaines de lignes, mesurees mot a mot contre ES
// par `tests/compat/sonde_langues.py`.

/// `GermanLightStemmer` de Lucene (`light_german`).
pub fn german_light(mot: &str) -> String {
    let mut s: Vec<char> = mot
        .chars()
        .map(|c| match c {
            'ä' | 'à' | 'á' | 'â' => 'a',
            'ö' | 'ò' | 'ó' | 'ô' => 'o',
            'ï' | 'ì' | 'í' | 'î' => 'i',
            'ü' | 'ù' | 'ú' | 'û' => 'u',
            autre => autre,
        })
        .collect();
    // `stEnding` : les consonnes derriere lesquelles un `s` final tombe.
    fn st(c: char) -> bool {
        matches!(c, 'b' | 'd' | 'f' | 'g' | 'h' | 'k' | 'l' | 'm' | 'n' | 't')
    }
    let mut n = s.len();
    // step1
    if n > 5 && s[n - 3] == 'e' && s[n - 2] == 'r' && s[n - 1] == 'n' {
        n -= 3;
    } else if n > 4 && s[n - 2] == 'e' && matches!(s[n - 1], 'm' | 'n' | 'r' | 's') {
        n -= 2;
    // Les deux dernieres regles de `step1` retirent une lettre chacune : un
    // `e` final, ou un `s` final derriere une consonne de `stEnding`.
    } else if n > 3 && (s[n - 1] == 'e' || (s[n - 1] == 's' && st(s[n - 2]))) {
        n -= 1;
    }
    // step2
    if n > 5 && s[n - 3] == 'e' && s[n - 2] == 's' && s[n - 1] == 't' {
        n -= 3;
    // Meme forme dans `step2` : `er` / `en`, ou `st` derriere une consonne.
    } else if n > 4
        && ((s[n - 2] == 'e' && matches!(s[n - 1], 'r' | 'n'))
            || (s[n - 2] == 's' && s[n - 1] == 't' && st(s[n - 3])))
    {
        n -= 2;
    }
    s.truncate(n);
    s.into_iter().collect()
}

/// Le repliement des accents commun aux trois stemmers legers latins.
fn replie_latin(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ä' => 'a',
        'ò' | 'ó' | 'ô' | 'ö' => 'o',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        autre => autre,
    }
}

/// `SpanishLightStemmer` de Lucene (`light_spanish`).
pub fn spanish_light(mot: &str) -> String {
    let mut s: Vec<char> = mot.chars().collect();
    if s.len() < 5 {
        return mot.to_string();
    }
    for c in s.iter_mut() {
        *c = replie_latin(*c);
    }
    let n = s.len();
    let garde = match s[n - 1] {
        'o' | 'a' | 'e' => n - 1,
        's' => {
            if s[n - 2] == 'e' && s[n - 3] == 's' && s[n - 4] == 'e' {
                n - 2
            } else if s[n - 2] == 'e' && s[n - 3] == 'c' {
                s[n - 3] = 'z';
                n - 2
            } else if matches!(s[n - 2], 'o' | 'a' | 'e') {
                n - 2
            } else {
                n
            }
        }
        _ => n,
    };
    s.truncate(garde);
    s.into_iter().collect()
}

/// `ItalianLightStemmer` de Lucene (`light_italian`).
pub fn italian_light(mot: &str) -> String {
    let mut s: Vec<char> = mot.chars().collect();
    if s.len() < 6 {
        return mot.to_string();
    }
    for c in s.iter_mut() {
        *c = replie_latin(*c);
    }
    let n = s.len();
    let garde = match (s[n - 1], s[n - 2]) {
        ('e', 'i') | ('e', 'h') => n - 2,
        ('e', _) => n - 1,
        ('i', 'h') | ('i', 'i') => n - 2,
        ('i', _) => n - 1,
        ('a', 'i') => n - 2,
        ('a', _) => n - 1,
        ('o', 'i') => n - 2,
        ('o', _) => n - 1,
        _ => n,
    };
    s.truncate(garde);
    s.into_iter().collect()
}

/// `PortugueseLightStemmer` de Lucene (`light_portuguese`).
///
/// Le seul des quatre a reecrire des lettres en cours de route (`ns` -> `m`,
/// `ões` -> `ão`, le feminin ramene au masculin), et le seul dont le
/// repliement des accents vient **apres** les coupes — donc le `ã` qu'il vient
/// de poser est replie juste apres.
pub fn portuguese_light(mot: &str) -> String {
    let mut s: Vec<char> = mot.chars().collect();
    if s.len() < 4 {
        return mot.to_string();
    }
    let mut n = retire_suffixe(&mut s);
    if n > 3 && s[n - 1] == 'a' {
        n = normalise_feminin(&mut s, n);
    }
    if n > 4 && matches!(s[n - 1], 'e' | 'a' | 'o') {
        n -= 1;
    }
    s.truncate(n);
    for c in s.iter_mut() {
        *c = match *c {
            'ã' => 'a',
            'õ' => 'o',
            'ç' => 'c',
            autre => replie_latin(autre),
        };
    }
    s.into_iter().collect()
}

fn finit_a(s: &[char], n: usize, suffixe: &str) -> bool {
    let f: Vec<char> = suffixe.chars().collect();
    n >= f.len() && s[n - f.len()..n] == f[..]
}

fn retire_suffixe(s: &mut [char]) -> usize {
    let n = s.len();
    if n > 4 && finit_a(s, n, "es") && matches!(s[n - 3], 'r' | 's' | 'l' | 'z') {
        return n - 2;
    }
    if n > 3 && finit_a(s, n, "ns") {
        s[n - 2] = 'm';
        return n - 1;
    }
    if n > 4 && (finit_a(s, n, "eis") || finit_a(s, n, "éis")) {
        s[n - 3] = 'e';
        s[n - 2] = 'l';
        return n - 1;
    }
    if n > 4 && finit_a(s, n, "ais") {
        s[n - 2] = 'l';
        return n - 1;
    }
    if n > 4 && finit_a(s, n, "óis") {
        s[n - 3] = 'o';
        s[n - 2] = 'l';
        return n - 1;
    }
    if n > 4 && finit_a(s, n, "is") {
        s[n - 1] = 'l';
        return n;
    }
    if n > 3 && (finit_a(s, n, "ões") || finit_a(s, n, "ães")) {
        let n = n - 1;
        s[n - 2] = 'ã';
        s[n - 1] = 'o';
        return n;
    }
    if n > 6 && finit_a(s, n, "mente") {
        return n - 5;
    }
    if n > 3 && s[n - 1] == 's' {
        return n - 1;
    }
    n
}

fn normalise_feminin(s: &mut [char], n: usize) -> usize {
    if n > 7 && (finit_a(s, n, "inha") || finit_a(s, n, "iaca") || finit_a(s, n, "eira")) {
        s[n - 1] = 'o';
        return n;
    }
    if n > 6 {
        for suffixe in ["osa", "ica", "ida", "ada", "iva", "ama"] {
            if finit_a(s, n, suffixe) {
                s[n - 1] = 'o';
                return n;
            }
        }
        if finit_a(s, n, "ona") {
            s[n - 3] = 'ã';
            s[n - 2] = 'o';
            return n - 1;
        }
        if finit_a(s, n, "ora") {
            return n - 1;
        }
        if finit_a(s, n, "esa") {
            s[n - 3] = 'ê';
            return n - 1;
        }
        if finit_a(s, n, "na") {
            s[n - 1] = 'o';
            return n;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porter_sur_les_cas_de_reference() {
        // Les exemples de l'article de Porter lui-meme.
        for (mot, attendu) in [
            ("caresses", "caress"),
            ("ponies", "poni"),
            ("ties", "ti"),
            ("caress", "caress"),
            ("cats", "cat"),
            ("feed", "feed"),
            ("agreed", "agre"),
            ("plastered", "plaster"),
            ("motoring", "motor"),
            ("sing", "sing"),
            ("conflated", "conflat"),
            ("troubled", "troubl"),
            ("hopping", "hop"),
            ("falling", "fall"),
            ("filing", "file"),
            ("happy", "happi"),
            ("relational", "relat"),
            ("rational", "ration"),
            ("valenci", "valenc"),
            ("digitizer", "digit"),
            ("conformabli", "conform"),
            ("radicalli", "radic"),
            ("differentli", "differ"),
            ("vileli", "vile"),
            ("analogousli", "analog"),
            ("vietnamization", "vietnam"),
            ("predication", "predic"),
            ("operator", "oper"),
            ("feudalism", "feudal"),
            ("decisiveness", "decis"),
            ("hopefulness", "hope"),
            ("callousness", "callous"),
            ("formaliti", "formal"),
            ("sensitiviti", "sensit"),
            ("sensibiliti", "sensibl"),
            ("triplicate", "triplic"),
            ("formative", "form"),
            ("formalize", "formal"),
            ("electriciti", "electr"),
            ("electrical", "electr"),
            ("hopeful", "hope"),
            ("goodness", "good"),
            ("revival", "reviv"),
            ("allowance", "allow"),
            ("inference", "infer"),
            ("airliner", "airlin"),
            ("gyroscopic", "gyroscop"),
            ("adjustable", "adjust"),
            ("defensible", "defens"),
            ("irritant", "irrit"),
            ("replacement", "replac"),
            ("adjustment", "adjust"),
            ("dependent", "depend"),
            ("adoption", "adopt"),
            ("homologou", "homolog"),
            ("communism", "commun"),
            ("activate", "activ"),
            ("angulariti", "angular"),
            ("homologous", "homolog"),
            ("effective", "effect"),
            ("bowdlerize", "bowdler"),
            ("probate", "probat"),
            ("rate", "rate"),
            ("cease", "ceas"),
            ("controll", "control"),
            ("roll", "roll"),
        ] {
            assert_eq!(porter(mot), attendu, "porter({mot})");
        }
    }
}
