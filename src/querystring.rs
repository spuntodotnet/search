//! Le mini-langage de `query_string` et de `simple_query_string`.
//!
//! C'est la clause qu'envoie tout ce qui laisse quelqu'un **ecrire** sa requete
//! — la barre de Kibana, un panneau Grafana, un filtre « recherche avancee ».
//! Son contenu n'est pas du JSON : c'est une grammaire, celle du `QueryParser`
//! classique de Lucene pour `query_string`, celle du `SimpleQueryParser` pour
//! `simple_query_string`.
//!
//! Contrat du module, celui du depot : **une construction qui n'est pas
//! reproduite est refusee en la nommant**. Un parseur qui accepte une
//! expression et l'interprete autrement qu'ES rend silencieusement les mauvais
//! documents — le pire resultat possible ici, puisque rien dans la reponse ne
//! le signale.
//!
//! Trois couches, dans cet ordre :
//!
//! 1. le **lexer**, porte des regles de `QueryParser.jj` — ses etats
//!    (`DEFAULT`, `Boost`, `Range`), son plus-long-match et son ordre de
//!    declaration en cas d'egalite. C'est lui qui decide qu'un `chat^x` est une
//!    erreur *lexicale* et qu'un `and` minuscule est un terme ;
//! 2. la **grammaire**, portee de la meme source : `Query`, `Clause`, `Term`,
//!    et surtout `addClause`, dont les quatre lignes decident de ce que
//!    `a AND b OR c` veut dire ;
//! 3. la **traduction** vers le Query DSL que ferrite sait deja executer. Rien
//!    n'est reimplemente ici : chaque feuille devient une clause JSON
//!    (`match`, `range`, `wildcard`...) passee a [`crate::dsl::build_query`],
//!    donc elle herite du repli sur les colonnes d'un `index: false`, des
//!    champs de metadonnees et de la tolerance aux champs non mappes.
//!
//! Toutes les regles qui suivent sont **mesurees** contre un ES 8.15 par
//! `tests/compat/diff_query_string.py`, jamais deduites de la documentation.

use serde_json::{json, Map, Value};
use tantivy::query::{Occur, Query};

use crate::dsl::{self, QueryCtx};
use crate::error::{EsError, EsResult};
use crate::mapping::FieldKind;

// ---------------------------------------------------------------------------
// 1. Le lexer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Jeton {
    And,
    Or,
    Not,
    Plus,
    Minus,
    /// `+`, `-` ou `!` suivi d'un blanc : Lucene en fait un **terme**.
    BareOper(char),
    LParen,
    RParen,
    Colon,
    Star,
    /// `^` et le nombre qui le suit — le lexer d'ES n'accepte rien d'autre
    /// apres un accent circonflexe, pas meme un blanc.
    Carat(f32),
    /// Le contenu entre guillemets, echappements intacts.
    Quoted(String),
    Term(String),
    /// Le texte apres `~` : vide, ou un nombre.
    FuzzySlop(String),
    PrefixTerm(String),
    WildTerm(String),
    RegexpTerm(String),
    /// `[` (inclusif) ou `{`.
    RangeStart(bool),
    RangeGoop(String),
    RangeQuoted(String),
    RangeTo,
    /// `]` (inclusif) ou `}`.
    RangeEnd(bool),
}

/// Les caracteres qu'un terme ne peut pas porter en tete (`_TERM_START_CHAR`).
fn debut_de_terme(c: char) -> bool {
    !matches!(
        c,
        ' ' | '\t'
            | '\n'
            | '\r'
            | '\u{3000}'
            | '+'
            | '-'
            | '!'
            | '('
            | ')'
            | ':'
            | '^'
            | '['
            | ']'
            | '"'
            | '{'
            | '}'
            | '~'
            | '*'
            | '?'
            | '\\'
            | '/'
    )
}

/// `_TERM_CHAR` : comme ci-dessus, plus `-` et `+`. C'est ce qui fait de
/// `chat-huant` **un** terme.
fn corps_de_terme(c: char) -> bool {
    debut_de_terme(c) || c == '-' || c == '+'
}

fn est_blanc(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{3000}')
}

/// Longueur (en `char`) du caractere de terme a cette position, 0 s'il n'y en a
/// pas. Un `\x` en fait deux.
fn long_car(c: &[char], i: usize, debut: bool) -> usize {
    match c.get(i) {
        None => 0,
        Some('\\') if i + 1 < c.len() => 2,
        Some(&x) if debut && debut_de_terme(x) => 1,
        Some(&x) if !debut && corps_de_terme(x) => 1,
        _ => 0,
    }
}

fn long_terme(c: &[char], i: usize) -> usize {
    let mut j = i;
    let l = long_car(c, j, true);
    if l == 0 {
        return 0;
    }
    j += l;
    loop {
        let l = long_car(c, j, false);
        if l == 0 {
            return j - i;
        }
        j += l;
    }
}

fn long_prefixterm(c: &[char], i: usize) -> usize {
    if c.get(i) == Some(&'*') {
        return 1;
    }
    let l = long_terme(c, i);
    if l == 0 {
        return 0;
    }
    if c.get(i + l) == Some(&'*') {
        l + 1
    } else {
        0
    }
}

fn long_wildterm(c: &[char], i: usize) -> usize {
    let mut j = i;
    let l = if matches!(c.get(j), Some('*' | '?')) {
        1
    } else {
        long_car(c, j, true)
    };
    if l == 0 {
        return 0;
    }
    j += l;
    loop {
        let l = if matches!(c.get(j), Some('*' | '?')) {
            1
        } else {
            long_car(c, j, false)
        };
        if l == 0 {
            return j - i;
        }
        j += l;
    }
}

/// `"` (~["\\] | \\.)* `"`
fn long_quote(c: &[char], i: usize) -> usize {
    if c.get(i) != Some(&'"') {
        return 0;
    }
    let mut j = i + 1;
    while j < c.len() {
        match c[j] {
            '"' => return j + 1 - i,
            '\\' if j + 1 < c.len() => j += 2,
            _ => j += 1,
        }
    }
    0
}

/// `/` (~[/] | \\/)* `/`
fn long_regexp(c: &[char], i: usize) -> usize {
    if c.get(i) != Some(&'/') {
        return 0;
    }
    let mut j = i + 1;
    while j < c.len() {
        match c[j] {
            '/' => return j + 1 - i,
            '\\' if c.get(j + 1) == Some(&'/') => j += 2,
            _ => j += 1,
        }
    }
    0
}

/// `(chiffres ("." chiffres)?)?`
fn long_nombre(c: &[char], i: usize) -> usize {
    let mut j = i;
    while c.get(j).is_some_and(char::is_ascii_digit) {
        j += 1;
    }
    if j == i {
        return 0;
    }
    if c.get(j) == Some(&'.') && c.get(j + 1).is_some_and(char::is_ascii_digit) {
        j += 1;
        while c.get(j).is_some_and(char::is_ascii_digit) {
            j += 1;
        }
    }
    j - i
}

fn mot(c: &[char], i: usize, m: &str) -> bool {
    c[i..].starts_with(&m.chars().collect::<Vec<_>>()[..])
}

/// L'analyse lexicale, etat par etat. Rend `Err(())` sur une erreur lexicale —
/// ce qu'ES rend alors est un `Failed to parse query [...]`, comme pour une
/// erreur de grammaire, et c'est tout ce qu'un client lit.
#[allow(clippy::too_many_lines)]
fn lex(s: &str) -> Result<Vec<Jeton>, ()> {
    let c: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut dans_borne = false;
    while i < c.len() {
        if est_blanc(c[i]) {
            i += 1;
            continue;
        }
        if dans_borne {
            // Etat `Range` : les seuls jetons sont `TO`, les deux fermetures,
            // une valeur entre guillemets et le reste (`RANGE_GOOP`).
            match c[i] {
                ']' | '}' => {
                    out.push(Jeton::RangeEnd(c[i] == ']'));
                    dans_borne = false;
                    i += 1;
                }
                _ => {
                    let goop = {
                        let mut j = i;
                        while j < c.len() && !matches!(c[j], ' ' | ']' | '}') {
                            j += 1;
                        }
                        j - i
                    };
                    // `RANGE_QUOTED` est declare avant `RANGE_GOOP` : a
                    // longueur egale, il gagne.
                    let quote = long_quote(&c, i);
                    if quote >= goop && quote > 0 {
                        out.push(Jeton::RangeQuoted(c[i + 1..i + quote - 1].iter().collect()));
                        i += quote;
                    } else if goop == 2 && mot(&c, i, "TO") {
                        out.push(Jeton::RangeTo);
                        i += 2;
                    } else if goop > 0 {
                        out.push(Jeton::RangeGoop(c[i..i + goop].iter().collect()));
                        i += goop;
                    } else {
                        return Err(());
                    }
                }
            }
            continue;
        }

        // Etat `DEFAULT` : plus long match, l'ordre de declaration departage.
        let mut meilleur: Option<(usize, Jeton)> = None;
        let propose = |n: usize, j: Jeton, meilleur: &mut Option<(usize, Jeton)>| {
            if n > 0 && meilleur.as_ref().is_none_or(|(m, _)| n > *m) {
                *meilleur = Some((n, j));
            }
        };
        if mot(&c, i, "AND") {
            propose(3, Jeton::And, &mut meilleur);
        }
        if mot(&c, i, "&&") {
            propose(2, Jeton::And, &mut meilleur);
        }
        if mot(&c, i, "OR") {
            propose(2, Jeton::Or, &mut meilleur);
        }
        if mot(&c, i, "||") {
            propose(2, Jeton::Or, &mut meilleur);
        }
        if mot(&c, i, "NOT") {
            propose(3, Jeton::Not, &mut meilleur);
        }
        if c[i] == '!' {
            propose(1, Jeton::Not, &mut meilleur);
        }
        if c[i] == '+' {
            propose(1, Jeton::Plus, &mut meilleur);
        }
        if c[i] == '-' {
            propose(1, Jeton::Minus, &mut meilleur);
        }
        if matches!(c[i], '+' | '-' | '!') && c.get(i + 1).copied().is_some_and(est_blanc) {
            propose(2, Jeton::BareOper(c[i]), &mut meilleur);
        }
        for (car, jeton) in [
            ('(', Jeton::LParen),
            (')', Jeton::RParen),
            (':', Jeton::Colon),
            ('*', Jeton::Star),
        ] {
            if c[i] == car {
                propose(1, jeton, &mut meilleur);
            }
        }
        if c[i] == '^' {
            // L'etat `Boost` ne saute pas les blancs et n'accepte qu'un
            // nombre : `a^ 2`, `a^x` et `a^` sont des erreurs lexicales.
            let n = long_nombre(&c, i + 1);
            if n == 0 {
                return Err(());
            }
            let txt: String = c[i + 1..i + 1 + n].iter().collect();
            propose(
                1 + n,
                Jeton::Carat(txt.parse().map_err(|_| ())?),
                &mut meilleur,
            );
        }
        let n = long_quote(&c, i);
        if n > 0 {
            propose(
                n,
                Jeton::Quoted(c[i + 1..i + n - 1].iter().collect()),
                &mut meilleur,
            );
        }
        let n = long_terme(&c, i);
        if n > 0 {
            propose(n, Jeton::Term(c[i..i + n].iter().collect()), &mut meilleur);
        }
        if c[i] == '~' {
            let n = long_nombre(&c, i + 1);
            propose(
                1 + n,
                Jeton::FuzzySlop(c[i + 1..i + 1 + n].iter().collect()),
                &mut meilleur,
            );
        }
        let n = long_prefixterm(&c, i);
        if n > 0 {
            propose(
                n,
                Jeton::PrefixTerm(c[i..i + n].iter().collect()),
                &mut meilleur,
            );
        }
        let n = long_wildterm(&c, i);
        if n > 0 {
            propose(
                n,
                Jeton::WildTerm(c[i..i + n].iter().collect()),
                &mut meilleur,
            );
        }
        let n = long_regexp(&c, i);
        if n > 0 {
            propose(
                n,
                Jeton::RegexpTerm(c[i..i + n].iter().collect()),
                &mut meilleur,
            );
        }
        if matches!(c[i], '[' | '{') {
            propose(1, Jeton::RangeStart(c[i] == '['), &mut meilleur);
        }
        match meilleur {
            None => return Err(()),
            Some((n, j)) => {
                if matches!(j, Jeton::RangeStart(_)) {
                    dans_borne = true;
                }
                out.push(j);
                i += n;
            }
        }
    }
    if dans_borne {
        return Err(());
    }
    Ok(out)
}

/// Retire les `\` d'echappement (`discardEscapeChar` de Lucene).
fn desechappe(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut echappe = false;
    for c in s.chars() {
        if echappe {
            out.push(c);
            echappe = false;
        } else if c == '\\' {
            echappe = true;
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 2. La grammaire
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Feuille {
    /// Un mot (ou une expression entre guillemets) a analyser.
    Texte {
        texte: String,
        phrase: bool,
        slop: u32,
    },
    /// Un motif a jokers, echappements **intacts** — `WildcardQuery` les lit.
    Joker(String),
    Prefixe(String),
    Regexp(String),
    Flou {
        texte: String,
        distance: Option<String>,
    },
    Borne {
        bas: Option<String>,
        haut: Option<String>,
        bas_inclus: bool,
        haut_inclus: bool,
    },
    /// `champ:*` — l'existence du champ. Sur le champ `*`, c'est `match_all`.
    Tous,
}

#[derive(Debug, Clone)]
enum Noeud {
    Feuille {
        champ: Option<String>,
        feuille: Feuille,
    },
    /// Les clauses **telles que la grammaire les a lues** : leur conjonction et
    /// leur modificateur, pas encore leur `Occur`.
    ///
    /// Le pliage est fait a la traduction, et pas avant, pour une raison qui
    /// decide du resultat : Lucene **n'ajoute pas** une clause dont l'analyzer
    /// n'a tire aucun terme (`if (q == null) return;`), mais il a deja rendu la
    /// precedente obligatoire. `chat AND ...` vaut donc `chat`, pas
    /// `+chat +rien`. Savoir si une clause produit quelque chose demande le
    /// mapping : c'est une decision de traduction.
    Bool(Vec<(Conj, Modif, Noeud)>),
    /// `^n` sur un terme, une phrase ou un groupe.
    Boost(Box<Noeud>, f32),
    /// `-x` de `simple_query_string` : « tous les documents sauf ceux-la »,
    /// pose comme une clause a part entiere (mesure : le score des documents
    /// rendus vaut 1.0, et la clause s'additionne aux autres).
    Negation(Box<Noeud>),
    /// L'arbre de `simple_query_string` : un booleen dont **toutes** les
    /// clauses portent le meme `Occur`.
    ///
    /// Sa forme n'est pas celle de `query_string`, et c'est la difference qui
    /// decide du resultat : `a b + c` y vaut `(a OU b) ET c` — un arbre
    /// binaire, construit de gauche a droite — la ou `query_string` en ferait
    /// une liste plate. Un operateur qui repete celui du sommet **allonge** ce
    /// sommet au lieu de l'emboiter (`a + b + c` rend trois clauses
    /// obligatoires, pas deux niveaux) : mesure contre ES 8.15.
    Combine { occur: Occur, clauses: Vec<Noeud> },
}

/// Le `buildQueryTree` de Lucene : la branche rejoint le sommet, et l'allonge
/// quand elle y trouve le meme operateur.
fn fusionne(sommet: Noeud, branche: Noeud, et: bool) -> Noeud {
    let occur = if et { Occur::Must } else { Occur::Should };
    if let Noeud::Combine {
        occur: o,
        mut clauses,
    } = sommet
    {
        if o == occur {
            clauses.push(branche);
            return Noeud::Combine { occur: o, clauses };
        }
        return Noeud::Combine {
            occur,
            clauses: vec![Noeud::Combine { occur: o, clauses }, branche],
        };
    }
    Noeud::Combine {
        occur,
        clauses: vec![sommet, branche],
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Modif {
    Aucun,
    Requis,
    Interdit,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Conj {
    Aucune,
    Et,
    Ou,
}

/// `addClause` de Lucene, mot pour mot : c'est lui qui decide de la
/// **precedence**, et aucune de ses quatre lignes n'est intuitive.
///
/// `a AND b OR c` n'y est ni `(a AND b) OR c` ni `a AND (b OR c)` : c'est une
/// liste plate ou `AND` rend obligatoire la clause **precedente**. Et la
/// promotion de cette precedente a lieu **avant** de savoir si la clause
/// courante produit quoi que ce soit : c'est le seul ordre qui explique que
/// `chat AND ...` (ou `...` ne rend aucun terme) vaille `chat` et non zero.
fn occur_de(clauses: &mut [(Occur, Value)], conj: Conj, mods: Modif, op_et: bool) -> Occur {
    if let Some(dernier) = clauses.last_mut() {
        if conj == Conj::Et && dernier.0 != Occur::MustNot {
            dernier.0 = Occur::Must;
        }
        if op_et && conj == Conj::Ou && dernier.0 != Occur::MustNot {
            dernier.0 = Occur::Should;
        }
    }
    let interdit = mods == Modif::Interdit;
    let requis = if op_et {
        !interdit && conj != Conj::Ou
    } else {
        mods == Modif::Requis || (conj == Conj::Et && !interdit)
    };
    if interdit {
        Occur::MustNot
    } else if requis {
        Occur::Must
    } else {
        Occur::Should
    }
}

struct Parseur<'a> {
    j: &'a [Jeton],
    i: usize,
    /// `allow_leading_wildcard` : un motif qui commence par `*` ou `?` est une
    /// **erreur de syntaxe** quand il est refuse, pas un motif ignore.
    joker_en_tete: bool,
}

impl Parseur<'_> {
    fn peek(&self, n: usize) -> Option<&Jeton> {
        self.j.get(self.i + n)
    }

    fn debut_de_clause(&self) -> bool {
        matches!(
            self.peek(0),
            Some(
                Jeton::Term(_)
                    | Jeton::Star
                    | Jeton::PrefixTerm(_)
                    | Jeton::WildTerm(_)
                    | Jeton::RegexpTerm(_)
                    | Jeton::BareOper(_)
                    | Jeton::RangeStart(_)
                    | Jeton::Quoted(_)
                    | Jeton::LParen
            )
        )
    }

    fn modifiers(&mut self) -> Modif {
        match self.peek(0) {
            Some(Jeton::Plus) => {
                self.i += 1;
                Modif::Requis
            }
            Some(Jeton::Minus | Jeton::Not) => {
                self.i += 1;
                Modif::Interdit
            }
            _ => Modif::Aucun,
        }
    }

    fn conjonction(&mut self) -> Conj {
        match self.peek(0) {
            Some(Jeton::And) => {
                self.i += 1;
                Conj::Et
            }
            Some(Jeton::Or) => {
                self.i += 1;
                Conj::Ou
            }
            _ => Conj::Aucune,
        }
    }

    /// Combien de jetons `TERM` **consecutifs** forment un groupe.
    ///
    /// C'est le `split_on_whitespace: false` d'ES, fige depuis la 7.0 : deux
    /// mots separes par un blanc ne font pas deux clauses, ils font **une**
    /// clause dont le texte porte l'espace. La difference ne se voit pas sur un
    /// champ `text` (l'analyzer redecoupe), et elle decide de tout ailleurs :
    /// sur un `keyword`, `edition rapide` cherche le terme `edition rapide` ;
    /// sur un numerique ou une date, la chaine entiere est illisible, donc la
    /// clause ne correspond a rien au lieu d'en trouver une moitie.
    ///
    /// Le groupe s'arrete devant `AND`, `OR`, `^`, `~` et `:` — mesure, jeton
    /// par jeton, contre ES 8.15 : `a b AND c` n'en fait pas un, `a b NOT c`
    /// si. Rend 0 quand il y en a moins de deux : la clause suit alors son
    /// chemin ordinaire.
    fn groupe_de_termes(&self) -> usize {
        let mut n = 0;
        while matches!(self.peek(n), Some(Jeton::Term(_)))
            && !matches!(
                self.peek(n + 1),
                Some(Jeton::And | Jeton::Or | Jeton::Carat(_) | Jeton::FuzzySlop(_) | Jeton::Colon)
            )
        {
            n += 1;
        }
        if n < 2 {
            0
        } else {
            n
        }
    }

    /// Le groupe consomme, rendu comme une feuille sur les champs par defaut.
    fn multi_terme(&mut self, champ: Option<&str>, n: usize) -> Noeud {
        let mut mots = Vec::with_capacity(n);
        for _ in 0..n {
            if let Some(Jeton::Term(t)) = self.peek(0).cloned() {
                mots.push(t);
            }
            self.i += 1;
        }
        Noeud::Feuille {
            champ: champ.map(str::to_string),
            feuille: Feuille::Texte {
                texte: desechappe(&mots.join(" ")),
                phrase: false,
                slop: 0,
            },
        }
    }

    fn query(&mut self, champ: Option<&str>) -> Result<Noeud, ()> {
        let mut clauses: Vec<(Conj, Modif, Noeud)> = Vec::new();
        let (mods, q) = match self.groupe_de_termes() {
            0 => (self.modifiers(), self.clause(champ)?),
            n => (Modif::Aucun, self.multi_terme(champ, n)),
        };
        let seul_sans_modificateur = mods == Modif::Aucun;
        clauses.push((Conj::Aucune, mods, q));
        loop {
            let suite = matches!(
                self.peek(0),
                Some(Jeton::And | Jeton::Or | Jeton::Plus | Jeton::Minus | Jeton::Not)
            ) || self.debut_de_clause();
            if !suite {
                break;
            }
            if let n @ 1.. = self.groupe_de_termes() {
                clauses.push((Conj::Aucune, Modif::Aucun, self.multi_terme(champ, n)));
                continue;
            }
            let conj = self.conjonction();
            let mods = self.modifiers();
            let q = self.clause(champ)?;
            clauses.push((conj, mods, q));
        }
        if clauses.len() == 1 && seul_sans_modificateur {
            return Ok(clauses.pop().expect("une clause").2);
        }
        Ok(Noeud::Bool(clauses))
    }

    fn clause(&mut self, champ: Option<&str>) -> Result<Noeud, ()> {
        // `LOOKAHEAD(2)` : un terme (ou une etoile) suivi de `:` nomme le champ.
        let mut champ = champ.map(str::to_string);
        match (self.peek(0), self.peek(1)) {
            (Some(Jeton::Term(t)), Some(Jeton::Colon)) => {
                champ = Some(desechappe(t));
                self.i += 2;
            }
            (Some(Jeton::Star), Some(Jeton::Colon)) => {
                champ = Some("*".into());
                self.i += 2;
            }
            _ => {}
        }
        if self.peek(0) == Some(&Jeton::LParen) {
            self.i += 1;
            let q = self.query(champ.as_deref())?;
            if self.peek(0) != Some(&Jeton::RParen) {
                return Err(());
            }
            self.i += 1;
            if let Some(Jeton::Carat(b)) = self.peek(0).cloned() {
                self.i += 1;
                return Ok(Noeud::Boost(Box::new(q), b));
            }
            return Ok(q);
        }
        self.terme(champ)
    }

    fn opt_flou(&mut self) -> Option<String> {
        if let Some(Jeton::FuzzySlop(s)) = self.peek(0).cloned() {
            self.i += 1;
            return Some(s);
        }
        None
    }

    fn opt_boost(&mut self) -> Option<f32> {
        if let Some(Jeton::Carat(b)) = self.peek(0).cloned() {
            self.i += 1;
            return Some(b);
        }
        None
    }

    fn terme(&mut self, champ: Option<String>) -> Result<Noeud, ()> {
        let jeton = self.peek(0).cloned().ok_or(())?;
        let (feuille, boost) = match jeton {
            Jeton::RangeStart(bas_inclus) => {
                self.i += 1;
                let bas = self.goop()?;
                if self.peek(0) != Some(&Jeton::RangeTo) {
                    return Err(());
                }
                self.i += 1;
                let haut = self.goop()?;
                let haut_inclus = match self.peek(0) {
                    Some(&Jeton::RangeEnd(inc)) => {
                        self.i += 1;
                        inc
                    }
                    _ => return Err(()),
                };
                let borne = |v: String| if v == "*" { None } else { Some(desechappe(&v)) };
                (
                    Feuille::Borne {
                        bas: borne(bas),
                        haut: borne(haut),
                        bas_inclus,
                        haut_inclus,
                    },
                    self.opt_boost(),
                )
            }
            Jeton::Quoted(t) => {
                self.i += 1;
                let flou = self.opt_flou();
                let boost = self.opt_boost();
                let slop = flou
                    .as_deref()
                    .and_then(|s| s.parse::<f32>().ok())
                    .map_or(0, |f| f as u32);
                (
                    Feuille::Texte {
                        texte: desechappe(&t),
                        phrase: true,
                        slop,
                    },
                    boost,
                )
            }
            Jeton::Term(_)
            | Jeton::Star
            | Jeton::PrefixTerm(_)
            | Jeton::WildTerm(_)
            | Jeton::RegexpTerm(_)
            | Jeton::BareOper(_) => {
                self.i += 1;
                let mut flou = self.opt_flou();
                let boost = self.opt_boost();
                if boost.is_some() {
                    if let Some(f) = self.opt_flou() {
                        flou = Some(f);
                    }
                }
                // L'ordre de `handleBareTokenQuery` : le joker l'emporte sur le
                // prefixe, le prefixe sur la regexp, et le flou ne s'applique
                // qu'a ce qui n'est ni l'un ni l'autre (`cha*~` est un prefixe).
                let f = match jeton {
                    Jeton::Star => Feuille::Tous,
                    Jeton::WildTerm(t) => {
                        self.verifie_tete(&t)?;
                        Feuille::Joker(t)
                    }
                    Jeton::PrefixTerm(t) => {
                        self.verifie_tete(&t)?;
                        let sans_etoile: String = t[..t.len() - 1].to_string();
                        Feuille::Prefixe(desechappe(&sans_etoile))
                    }
                    Jeton::RegexpTerm(t) => Feuille::Regexp(t[1..t.len() - 1].to_string()),
                    // Un `BAREOPER` vaut son premier caractere, comme terme.
                    Jeton::Term(t) => texte_ou_flou(desechappe(&t), flou),
                    Jeton::BareOper(c) => texte_ou_flou(c.to_string(), flou),
                    _ => return Err(()),
                };
                (f, boost)
            }
            _ => return Err(()),
        };
        let n = Noeud::Feuille { champ, feuille };
        Ok(match boost {
            Some(b) => Noeud::Boost(Box::new(n), b),
            None => n,
        })
    }

    /// `allow_leading_wildcard: false` : un motif en tete rend une erreur de
    /// syntaxe (mesure contre ES 8.15 : `Failed to parse query [*hat]`).
    fn verifie_tete(&self, motif: &str) -> Result<(), ()> {
        if !self.joker_en_tete && (motif.starts_with('*') || motif.starts_with('?')) {
            return Err(());
        }
        Ok(())
    }

    fn goop(&mut self) -> Result<String, ()> {
        match self.peek(0).cloned() {
            Some(Jeton::RangeGoop(g) | Jeton::RangeQuoted(g)) => {
                self.i += 1;
                Ok(g)
            }
            Some(Jeton::RangeTo) => {
                self.i += 1;
                Ok("TO".into())
            }
            _ => Err(()),
        }
    }
}

/// Un mot nu : flou s'il porte un `~`, texte sinon.
fn texte_ou_flou(texte: String, flou: Option<String>) -> Feuille {
    match flou {
        Some(distance) => Feuille::Flou {
            texte,
            distance: Some(distance),
        },
        None => Feuille::Texte {
            texte,
            phrase: false,
            slop: 0,
        },
    }
}

fn analyse(expr: &str, joker_en_tete: bool) -> Result<Option<Noeud>, ()> {
    let jetons = lex(expr)?;
    if jetons.is_empty() {
        return Ok(None);
    }
    let mut p = Parseur {
        j: &jetons,
        i: 0,
        joker_en_tete,
    };
    let n = p.query(None)?;
    if p.i != jetons.len() {
        return Err(());
    }
    Ok(Some(n))
}

// ---------------------------------------------------------------------------
// 3. Les parametres
// ---------------------------------------------------------------------------

/// Ce que la clause declare, resolu une fois pour toutes.
struct Options {
    /// Les champs par defaut et leur ponderation, tels que declares (motifs
    /// compris). Vide veut dire `*`.
    defaut: Vec<(String, f32)>,
    op_et: bool,
    lenient: bool,
    analyse_jokers: bool,
    joker_en_tete: bool,
    tie_breaker: f32,
    fuzziness: Option<Value>,
    transpositions: bool,
    time_zone: Option<String>,
    msm: Option<Value>,
    boost: Option<Value>,
}

fn lit_bool(o: &Map<String, Value>, cle: &str, defaut: bool, clause: &str) -> EsResult<bool> {
    match o.get(cle) {
        None | Some(Value::Null) => Ok(defaut),
        Some(Value::Bool(b)) => Ok(*b),
        Some(Value::String(s)) if s == "true" => Ok(true),
        Some(Value::String(s)) if s == "false" => Ok(false),
        Some(v) => Err(EsError::illegal_argument(format!(
            "[{clause}] : [{cle}] attend un booleen (recu {v})"
        ))),
    }
}

/// Un parametre qu'ES sert et que ferrite ne reproduit pas : refuse en le
/// nommant, jamais accepte et ignore.
fn refuse_parametre(clause: &str, cle: &str, raison: &str) -> EsError {
    EsError::unsupported(format!(
        "ferrite ne supporte pas [{cle}] dans [{clause}] : {raison}"
    ))
}

/// Les parametres dont la **valeur par defaut** ne demande rien : les accepter
/// telle quelle n'est pas un echec silencieux, les accepter autrement en
/// serait un.
fn refuse_si_change(
    o: &Map<String, Value>,
    clause: &str,
    cle: &str,
    defaut: &Value,
    raison: &str,
) -> EsResult<()> {
    match o.get(cle) {
        None | Some(Value::Null) => Ok(()),
        Some(v) if v == defaut => Ok(()),
        Some(_) => Err(refuse_parametre(clause, cle, raison)),
    }
}

const REFUS_COMMUNS: &[(&str, &str)] = &[
    (
        "quote_field_suffix",
        "il fait chercher une expression entre guillemets dans un **autre** champ que celui \
         nomme, et une phrase posee sur le mauvais champ rend des documents plausibles et faux",
    ),
    (
        "analyzer",
        "il remplace l'analyzer du champ pour la requete seule ; ferrite analyse toujours une \
         requete avec le `search_analyzer` du champ vise, et servir le parametre a moitie \
         changerait les termes cherches sans le dire",
    ),
    (
        "quote_analyzer",
        "le meme, pour les expressions entre guillemets",
    ),
];

fn lit_options(o: &Map<String, Value>, clause: &str, permis: &[&str]) -> EsResult<Options> {
    for cle in o.keys() {
        if !permis.contains(&cle.as_str()) {
            // La phrase d'ES, mot pour mot : c'est celle qu'un client remonte.
            return Err(EsError::parsing(if clause == "query_string" {
                format!("[query_string] query does not support [{cle}]")
            } else {
                format!("[simple_query_string] unsupported field [{cle}]")
            }));
        }
    }
    for (cle, raison) in REFUS_COMMUNS {
        if o.contains_key(*cle) {
            return Err(refuse_parametre(clause, cle, raison));
        }
    }
    refuse_si_change(
        o,
        clause,
        "auto_generate_synonyms_phrase_query",
        &json!(true),
        "",
    )
    .ok();
    // `fields` vide vaut `fields` absent : le defaut redevient `*` (mesure).
    let mut defaut = Vec::new();
    if let Some(v) = o.get("fields") {
        let liste = v.as_array().ok_or_else(|| {
            EsError::illegal_argument(format!("[{clause}] : [fields] attend une liste"))
        })?;
        for spec in liste {
            let spec = spec.as_str().ok_or_else(|| {
                EsError::illegal_argument(format!("[{clause}.fields] : liste de chaines attendue"))
            })?;
            match spec.rsplit_once('^') {
                Some((nom, b)) if !nom.is_empty() => {
                    let poids = b.trim().parse::<f32>().map_err(|_| {
                        EsError::illegal_argument(format!(
                            "[{clause}.fields] : ponderation invalide dans [{spec}]"
                        ))
                    })?;
                    defaut.push((nom.to_string(), poids));
                }
                _ => defaut.push((spec.to_string(), 1.0)),
            }
        }
    }
    if defaut.is_empty() {
        if let Some(df) = o.get("default_field") {
            let df = df.as_str().ok_or_else(|| {
                EsError::illegal_argument(format!("[{clause}] : [default_field] attend une chaine"))
            })?;
            defaut.push((df.to_string(), 1.0));
        }
    }
    if defaut.is_empty() {
        defaut.push(("*".into(), 1.0));
    }

    let op_et = match o.get("default_operator").and_then(Value::as_str) {
        None => false,
        Some(s) if s.eq_ignore_ascii_case("and") => true,
        Some(s) if s.eq_ignore_ascii_case("or") => false,
        Some(s) => {
            return Err(EsError::illegal_argument(format!(
                "No enum constant org.elasticsearch.index.query.Operator.{}",
                s.to_uppercase()
            )))
        }
    };

    // `lenient` a un defaut **qui se deduit des champs** : quand la clause ne
    // vise rien d'autre que `*`, ES le force a `true` — sans quoi taper un mot
    // ferait echouer la recherche entiere des qu'un champ numerique est dans
    // l'expansion. Mesure : `n:abc` rend 200 sans `default_field`, et 400 avec.
    let etoile_seule = defaut.len() == 1 && defaut[0].0 == "*";
    let lenient = match o.get("lenient") {
        None | Some(Value::Null) => etoile_seule,
        _ => lit_bool(o, "lenient", false, clause)?,
    };

    refuse_si_change(
        o,
        clause,
        "fuzzy_max_expansions",
        &json!(50),
        "ferrite ne supporte pas [max_expansions] dans [fuzzy] : l'automate de Levenshtein que \
         tantivy compile n'expose pas le nombre de termes developpes",
    )?;
    refuse_si_change(
        o,
        clause,
        "fuzzy_prefix_length",
        &json!(0),
        "ferrite ne supporte pas [prefix_length] dans [fuzzy] : le prefixe fige n'est pas expose \
         par l'automate de Levenshtein que tantivy compile",
    )?;

    Ok(Options {
        defaut,
        op_et,
        lenient,
        analyse_jokers: lit_bool(o, "analyze_wildcard", false, clause)?,
        joker_en_tete: lit_bool(o, "allow_leading_wildcard", true, clause)?,
        tie_breaker: match o.get("tie_breaker") {
            None => 0.0,
            Some(v) => v.as_f64().ok_or_else(|| {
                EsError::illegal_argument(format!("[{clause}] : [tie_breaker] attend un nombre"))
            })? as f32,
        },
        fuzziness: o.get("fuzziness").cloned(),
        transpositions: lit_bool(o, "fuzzy_transpositions", true, clause)?,
        time_zone: o
            .get("time_zone")
            .and_then(Value::as_str)
            .map(str::to_string),
        msm: o.get("minimum_should_match").cloned(),
        boost: o.get("boost").cloned(),
    })
}

// ---------------------------------------------------------------------------
// 4. La traduction
// ---------------------------------------------------------------------------

/// Les champs qu'un motif designe.
///
/// Un motif s'etend sur les feuilles **du mapping** — multi-fields et
/// sous-champs d'objet compris, exactement comme ES (mesure). Les sous-champs
/// d'un `nested` en sont ecartes : chez ES ils visent les documents caches, que
/// le filtre de parent elimine ensuite, donc ils ne rendent jamais rien ; les
/// garder ici les ferait refuser, ce qui serait pire que ce qu'ES fait.
fn etend(ctx: &QueryCtx, motif: &str) -> Vec<String> {
    if !motif.contains('*') {
        return vec![motif.to_string()];
    }
    ctx.fields
        .mapped
        .keys()
        .filter(|nom| !nom.starts_with('_'))
        .filter(|nom| ctx.fields.racine_nested(nom).is_none())
        .filter(|nom| crate::search::glob_match(motif, nom))
        .cloned()
        .collect()
}

/// Le contexte de traduction : les options, plus ce que la clause a nomme.
struct Traducteur<'a, 'b> {
    ctx: &'a QueryCtx<'b>,
    opts: &'a Options,
    clause: &'static str,
    /// Le `minimum_should_match` a poser sur la feuille **racine**, quand toute
    /// l'expression tient en une seule clause sur un seul champ.
    msm_de_la_racine: Option<Value>,
}

impl Traducteur<'_, '_> {
    /// La liste des champs a interroger pour une feuille, avec leur poids.
    fn champs(&self, nomme: Option<&str>) -> Vec<(String, f32)> {
        match nomme {
            // Un champ nomme dans l'expression l'emporte sur `fields` et sur
            // `default_field` (mesure : `titre:chat` avec `fields: ["absent"]`
            // cherche bien dans `titre`).
            Some(n) => etend(self.ctx, n).into_iter().map(|c| (c, 1.0)).collect(),
            None => self
                .opts
                .defaut
                .iter()
                .flat_map(|(motif, poids)| {
                    etend(self.ctx, motif).into_iter().map(move |c| (c, *poids))
                })
                .collect(),
        }
    }

    /// Analyse un motif a jokers comme ES le fait : chaque morceau litteral est
    /// **normalise** (le defaut) ou **analyse** en entier (`analyze_wildcard`),
    /// les jokers et les caracteres echappes passant tels quels.
    fn motif(&self, champ: &str, motif: &str) -> EsResult<String> {
        let Ok(mf) = self.ctx.field(champ, self.clause) else {
            return Ok(motif.to_string());
        };
        // Sur un champ qui n'est pas `text`, ES compare au terme **brut** : son
        // analyzer de recherche est `keyword`, qui ne replie rien. Mesure :
        // `tag:CH*` ne rend aucun document, `analyze_wildcard` ou pas.
        if mf.ty.kind() != FieldKind::Text {
            return Ok(motif.to_string());
        }
        let mut out = String::with_capacity(motif.len());
        let mut morceau = String::new();
        let mut chars = motif.chars().peekable();
        let vider = |morceau: &mut String, out: &mut String| -> EsResult<()> {
            if morceau.is_empty() {
                return Ok(());
            }
            let tokens = if self.opts.analyse_jokers {
                self.ctx.analyze(morceau, mf.search_analyzer)?
            } else {
                self.ctx.analyze_normalise(morceau, mf.search_analyzer)?
            };
            // Un morceau qui ne rend pas exactement un token reste tel quel :
            // ES ne saurait pas plus quoi en faire.
            let remplace = if tokens.len() == 1 {
                tokens[0].1.clone()
            } else {
                std::mem::take(morceau)
            };
            for c in remplace.chars() {
                if matches!(c, '*' | '?' | '\\') {
                    out.push('\\');
                }
                out.push(c);
            }
            morceau.clear();
            Ok(())
        };
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    // Un caractere echappe n'est pas normalise, et il garde son
                    // echappement : c'est un litteral pour l'automate.
                    vider(&mut morceau, &mut out)?;
                    out.push('\\');
                    if let Some(s) = chars.next() {
                        out.push(s);
                    }
                }
                '*' | '?' => {
                    vider(&mut morceau, &mut out)?;
                    out.push(c);
                }
                autre => morceau.push(autre),
            }
        }
        vider(&mut morceau, &mut out)?;
        Ok(out)
    }

    /// La clause JSON qu'une feuille devient sur **un** champ.
    fn json(&self, champ: &str, feuille: &Feuille) -> EsResult<Value> {
        Ok(match feuille {
            // `champ:>5` : le raccourci de borne. Il se lit **apres**
            // desechappement (`n:\>5` est une borne aussi, mesure) et il exige
            // une valeur derriere — `n:>` cherche le terme `>`.
            Feuille::Texte {
                texte,
                phrase: false,
                ..
            } if borne_courte(texte).is_some() => {
                let (cle, valeur) = borne_courte(texte).expect("teste juste au-dessus");
                let mut spec = Map::new();
                spec.insert(cle.into(), json!(valeur));
                if let Some(tz) = &self.opts.time_zone {
                    spec.insert("time_zone".into(), json!(tz));
                }
                json!({ "range": { champ: Value::Object(spec) } })
            }
            Feuille::Texte {
                texte,
                phrase: false,
                ..
            } => {
                let mut spec = Map::new();
                spec.insert("query".into(), json!(texte));
                spec.insert(
                    "operator".into(),
                    json!(if self.opts.op_et { "and" } else { "or" }),
                );
                // `minimum_should_match` s'applique a la requete que le parseur
                // rend, **quelle qu'elle soit** : quand toute l'expression tient
                // en un groupe de mots sur un seul champ, cette requete est le
                // booleen des termes analyses, et le minimum y descend. Sur
                // plusieurs champs la racine est un `dis_max`, et ES ne
                // l'applique alors pas (mesure).
                if let Some(msm) = &self.msm_de_la_racine {
                    spec.insert("minimum_should_match".into(), msm.clone());
                }
                json!({ "match": { champ: Value::Object(spec) } })
            }
            Feuille::Texte {
                texte,
                phrase: true,
                slop,
            } => json!({"match_phrase": {champ: {"query": texte, "slop": slop}}}),
            Feuille::Joker(motif) => {
                json!({"wildcard": {champ: {"value": self.motif(champ, motif)?}}})
            }
            Feuille::Prefixe(p) => {
                // `motif` attend un motif : un prefixe litteral s'y echappe.
                let echappe: String = p
                    .chars()
                    .flat_map(|c| {
                        if matches!(c, '*' | '?' | '\\') {
                            vec!['\\', c]
                        } else {
                            vec![c]
                        }
                    })
                    .collect();
                let normalise = self.motif(champ, &echappe)?;
                json!({"prefix": {champ: {"value": desechappe(&normalise)}}})
            }
            Feuille::Regexp(m) => json!({"regexp": {champ: {"value": m}}}),
            Feuille::Flou { texte, distance } => {
                let mut spec = Map::new();
                spec.insert("value".into(), json!(texte));
                spec.insert("transpositions".into(), json!(self.opts.transpositions));
                // `~` nu prend `fuzziness` (AUTO par defaut) ; `~n` impose n, et
                // ES refuse tout ce qui n'est pas 0, 1 ou 2 — avec deux phrases
                // differentes selon que n est entier ou non.
                match distance.as_deref() {
                    Some("") | None => {
                        if let Some(f) = &self.opts.fuzziness {
                            spec.insert("fuzziness".into(), f.clone());
                        }
                    }
                    Some(n) => {
                        let v: f32 = n.parse().map_err(|_| {
                            EsError::illegal_argument(format!("[{n}] : distance invalide"))
                        })?;
                        #[allow(clippy::float_cmp)]
                        if v != v.trunc() {
                            return Err(erreur_de_shard(format!(
                                "failed to create query: fuzziness needs to be one of 0.0, 1.0 or \
                                 2.0 but was {n}"
                            )));
                        }
                        let d = v as i64;
                        if !(0..=2).contains(&d) {
                            return Err(erreur_de_shard(format!(
                                "failed to create query: Valid edit distances are [0, 1, 2] but \
                                 was [{d}]"
                            )));
                        }
                        spec.insert("fuzziness".into(), json!(d));
                    }
                }
                json!({ "fuzzy": { champ: Value::Object(spec) } })
            }
            Feuille::Borne {
                bas,
                haut,
                bas_inclus,
                haut_inclus,
            } => {
                // `[* TO *]` ne borne rien : c'est l'existence du champ, et
                // c'est ce qu'ES en fait (un intervalle ouvert des deux cotes).
                if bas.is_none() && haut.is_none() {
                    return Ok(json!({ "exists": { "field": champ } }));
                }
                let mut spec = Map::new();
                if let Some(b) = bas {
                    spec.insert(if *bas_inclus { "gte" } else { "gt" }.into(), json!(b));
                }
                if let Some(h) = haut {
                    spec.insert(if *haut_inclus { "lte" } else { "lt" }.into(), json!(h));
                }
                if let Some(tz) = &self.opts.time_zone {
                    spec.insert("time_zone".into(), json!(tz));
                }
                json!({ "range": { champ: Value::Object(spec) } })
            }
            Feuille::Tous => json!({ "exists": { "field": champ } }),
        })
    }

    /// Cette feuille tire-t-elle **un terme** de ce champ ?
    ///
    /// C'est la question que Lucene pose avant d'ajouter une clause : un mot
    /// dont l'analyzer ne rend rien (un mot vide, une suite de ponctuation) ne
    /// devient pas « ne correspond a rien », il **disparait** de la requete.
    fn produit_un_terme(&self, champ: &str, feuille: &Feuille) -> EsResult<bool> {
        let (Feuille::Texte { texte, .. }, Ok(mf)) = (feuille, self.ctx.field(champ, self.clause))
        else {
            return Ok(true);
        };
        if mf.ty.kind() != FieldKind::Text || borne_courte(texte).is_some() {
            return Ok(true);
        }
        Ok(!self.ctx.analyze(texte, mf.search_analyzer)?.is_empty())
    }

    /// La clause DSL d'une feuille, ou `None` quand elle n'en produit aucune —
    /// le `null` de Lucene, qui n'est pas « aucun document ».
    fn feuille(&self, nomme: Option<&str>, feuille: &Feuille) -> EsResult<Option<Value>> {
        // `_exists_:champ` n'est pas un champ mais une clause : ES lit le
        // **terme** comme un nom de champ et rend son `exists`.
        if nomme == Some(CHAMP_EXISTE) {
            if let Feuille::Texte { texte, .. } = feuille {
                let v = json!({"exists": {"field": texte}});
                return match dsl::build_query(&v, self.ctx) {
                    Ok(_) => Ok(Some(v)),
                    Err(e) if self.ctx.champ_inconnu_tolere(&e) => {
                        Ok(Some(json!({"match_none": {}})))
                    }
                    Err(e) => Err(sur_le_shard(e)),
                };
            }
        }
        // `*:*` — et `*` tout court quand aucun champ n'est nomme et que la
        // clause n'a pas de `default_field` : tous les documents.
        if matches!(feuille, Feuille::Tous) && nomme.is_none_or(|n| n == "*") {
            let seulement_etoile = self.opts.defaut.len() == 1 && self.opts.defaut[0].0 == "*";
            if nomme == Some("*") || seulement_etoile {
                return Ok(Some(json!({"match_all": {}})));
            }
        }
        let champs = self.champs(nomme);
        // Un motif de champ qui ne designe rien rend chez ES un
        // `MatchNoDocsQuery("unmapped fields [x]")`, pas un `null` : la clause
        // est bien posee, et elle ne correspond a rien.
        let mut ecarte = champs.is_empty();
        let mut subs: Vec<Value> = Vec::new();
        for (champ, poids) in champs {
            if !self.produit_un_terme(&champ, feuille)? {
                continue;
            }
            let v = self.json(&champ, feuille)?;
            // La clause est **construite** pour savoir si ce champ la sert :
            // c'est la seule facon de distinguer « ES ecarte ce champ » de
            // « ES refuse la requete ». Le resultat est jete, la clause rendue
            // est celle qui repassera par le traducteur du DSL.
            match dsl::build_query(&v, self.ctx) {
                Ok(_) => {}
                // Un champ que ce mapping ne connait pas ne correspond a rien :
                // c'est le `MatchNoDocsQuery("unmapped fields [x]")` d'ES.
                Err(e) if self.ctx.champ_inconnu_tolere(&e) => {
                    ecarte = true;
                    continue;
                }
                // `lenient` : ce champ-la ne sait pas servir cette clause. ES y
                // pose un `MatchNoDocsQuery`, pas un `null` — et il attrape
                // **toute** exception du champ, pas seulement une valeur
                // illisible : `b:al*` sur une expansion `*` qui contient une
                // date rend 200 chez lui, en ecartant la date.
                //
                // La frontiere est ailleurs, et elle est la meme que partout
                // dans ce depot : un refus de **perimetre** de ferrite n'est
                // jamais avale. Sans cette moitie, `lenient: true` transformerait
                // chaque chose que ferrite ne sait pas faire en silence.
                Err(e) if self.opts.lenient && e.ty != crate::error::UNSUPPORTED => {
                    ecarte = true;
                    continue;
                }
                Err(e) => return Err(self.refus(&champ, feuille, e)),
            }
            #[allow(clippy::float_cmp)]
            subs.push(if poids == 1.0 {
                v
            } else {
                avec_boost(v, poids)
            });
        }
        Ok(match subs.len() {
            0 if ecarte => Some(json!({"match_none": {}})),
            0 => None,
            1 => Some(subs.pop().expect("une clause")),
            _ => Some(json!({"dis_max": {
                "queries": subs,
                "tie_breaker": self.opts.tie_breaker,
            }})),
        })
    }

    /// Le refus d'une feuille, avec la phrase qu'ES prononce.
    ///
    /// Elle est reprise mot pour mot la ou elle est mesuree : c'est celle que
    /// le client remonte dans son exception, et sur un champ numerique ou
    /// booleen elle nomme la valeur fautive — ce que la phrase de ferrite fait
    /// aussi, mais autrement.
    fn refus(&self, champ: &str, feuille: &Feuille, e: EsError) -> EsError {
        if !e.valeur_illisible {
            return sur_le_shard(e);
        }
        let valeur = match feuille {
            Feuille::Texte { texte, .. } | Feuille::Flou { texte, .. } => texte.as_str(),
            _ => return sur_le_shard(e),
        };
        match self.ctx.field(champ, self.clause).map(|mf| mf.ty.kind()) {
            Ok(FieldKind::I64 | FieldKind::F64) => erreur_de_shard(format!(
                "failed to create query: For input string: \"{valeur}\""
            )),
            Ok(FieldKind::Bool) => erreur_de_shard(format!(
                "failed to create query: Can't parse boolean value [{valeur}], expected [true] or \
                 [false]"
            )),
            _ => sur_le_shard(e),
        }
    }

    fn noeud(&self, n: &Noeud, racine: bool) -> EsResult<Option<Value>> {
        Ok(match n {
            Noeud::Feuille { champ, feuille } => self.feuille(champ.as_deref(), feuille)?,
            Noeud::Boost(inner, b) => self.noeud(inner, false)?.map(|q| avec_boost(q, *b)),
            Noeud::Negation(inner) => self
                .noeud(inner, false)?
                .map(|q| json!({"bool": {"must_not": [q], "should": [{"match_all": {}}]}})),
            // L'arbre de `simple_query_string` : toutes les clauses portent le
            // meme `Occur`, et une branche qui ne produit rien disparait.
            Noeud::Combine { occur, clauses } => {
                let construites: Vec<Value> = clauses
                    .iter()
                    .map(|c| self.noeud(c, false))
                    .collect::<EsResult<Vec<_>>>()?
                    .into_iter()
                    .flatten()
                    .collect();
                match construites.len() {
                    0 => return Ok(None),
                    1 => Some(construites.into_iter().next().expect("une clause")),
                    _ => {
                        let cle = if *occur == Occur::Must {
                            "must"
                        } else {
                            "should"
                        };
                        let mut corps = Map::new();
                        corps.insert(cle.into(), Value::Array(construites));
                        if racine {
                            if let Some(msm) = &self.opts.msm {
                                corps.insert("minimum_should_match".into(), msm.clone());
                            }
                        }
                        Some(json!({ "bool": Value::Object(corps) }))
                    }
                }
            }
            Noeud::Bool(clauses) => {
                let mut construites: Vec<(Occur, Value)> = Vec::new();
                let mut premier_nu = false;
                for (rang, (conj, mods, sous)) in clauses.iter().enumerate() {
                    // La promotion de la clause precedente a lieu **avant** la
                    // traduction de celle-ci : voir `occur_de`.
                    let occur = occur_de(&mut construites, *conj, *mods, self.opts.op_et);
                    let Some(q) = self.noeud(sous, false)? else {
                        continue;
                    };
                    if rang == 0 && *mods == Modif::Aucun {
                        premier_nu = true;
                    }
                    construites.push((occur, q));
                }
                if construites.is_empty() {
                    return Ok(None);
                }
                // `if (clauses.size() == 1 && firstQuery != null) return
                // firstQuery;` — et il compte les clauses **survivantes**. Ce
                // n'est pas cosmetique : `minimum_should_match` ne s'applique
                // qu'a un booleen, donc `chat … …` (ou `…` ne rend aucun terme)
                // avec un minimum de 2 rend les documents de `chat`, pas zero.
                if construites.len() == 1 && premier_nu {
                    return Ok(Some(construites.pop().expect("une clause").1));
                }
                // Le `bool` du DSL fait deja le reste : la clause positive
                // implicite d'un booleen purement negatif, et le « au moins un
                // `should` quand rien n'est obligatoire ».
                let mut corps = Map::new();
                for (cle, cherche) in [
                    ("must", Occur::Must),
                    ("should", Occur::Should),
                    ("must_not", Occur::MustNot),
                ] {
                    let liste: Vec<Value> = construites
                        .iter()
                        .filter(|(o, _)| *o == cherche)
                        .map(|(_, q)| q.clone())
                        .collect();
                    if !liste.is_empty() {
                        corps.insert(cle.into(), Value::Array(liste));
                    }
                }
                // `minimum_should_match` ne s'applique qu'a la **racine**, et
                // seulement si elle est un booleen : c'est ce que fait ES, qui
                // le pose sur la requete rendue par le parseur.
                if racine {
                    if let Some(msm) = &self.opts.msm {
                        corps.insert("minimum_should_match".into(), msm.clone());
                    }
                }
                Some(json!({ "bool": Value::Object(corps) }))
            }
        })
    }
}

/// Pose un `boost` sur une clause que ce module a fabriquee.
///
/// Toutes ont la meme forme — `{clause: {champ: {…}}}` ou `{clause: {…}}` —
/// et acceptent `boost` la ou ce module le pose.
fn avec_boost(v: Value, b: f32) -> Value {
    let Value::Object(mut o) = v else { return v };
    let Some(nom) = o.keys().next().cloned() else {
        return Value::Object(o);
    };
    let Some(Value::Object(mut corps)) = o.remove(&nom) else {
        return Value::Object(o);
    };
    // `exists`, `match_all`, `bool`, `dis_max` portent `boost` dans leur corps ;
    // les clauses de champ le portent dans l'objet **du champ**.
    let au_corps = matches!(
        nom.as_str(),
        "exists" | "match_all" | "match_none" | "bool" | "dis_max"
    );
    if au_corps {
        if nom == "match_none" {
            // `match_none` n'accepte aucun parametre chez ES comme ici, et
            // ponderer « rien » ne veut rien dire.
            return json!({ nom: Value::Object(corps) });
        }
        corps.insert("boost".into(), json!(b));
        return json!({ nom: Value::Object(corps) });
    }
    let Some(champ) = corps.keys().next().cloned() else {
        return json!({ nom: Value::Object(corps) });
    };
    match corps.remove(&champ) {
        Some(Value::Object(mut spec)) => {
            spec.insert("boost".into(), json!(b));
            corps.insert(champ, Value::Object(spec));
        }
        Some(autre) => {
            corps.insert(champ, autre);
        }
        None => {}
    }
    json!({ nom: Value::Object(corps) })
}

/// La clause ne vise-t-elle qu'un seul champ ? C'est la condition sous laquelle
/// la requete rendue par le parseur est un booleen de termes et non un
/// `dis_max` — donc celle sous laquelle `minimum_should_match` s'applique.
fn t_un_seul_champ(ctx: &QueryCtx, opts: &Options, nomme: Option<&str>) -> bool {
    match nomme {
        Some(n) => etend(ctx, n).len() == 1,
        None => opts.defaut.len() == 1 && etend(ctx, &opts.defaut[0].0).len() == 1,
    }
}

/// Le nom du champ que `_exists_:champ` interroge, chez ES.
const CHAMP_EXISTE: &str = "_exists_";

/// `champ:>5`, `champ:>=5`, `champ:<5`, `champ:<=5` — rend la cle de borne et
/// la valeur, ou `None` si ce n'en est pas une.
fn borne_courte(texte: &str) -> Option<(&'static str, &str)> {
    for (prefixe, cle) in [(">=", "gte"), ("<=", "lte"), (">", "gt"), ("<", "lt")] {
        if let Some(reste) = texte.strip_prefix(prefixe) {
            if !reste.is_empty() {
                return Some((cle, reste));
            }
        }
    }
    None
}

/// Une erreur que le **shard** prononce : c'est le type que porte tout refus
/// de `query_string`, y compris ceux qui viennent d'une valeur illisible.
fn erreur_de_shard(reason: impl Into<String>) -> EsError {
    EsError::new(
        axum::http::StatusCode::BAD_REQUEST,
        "query_shard_exception",
        reason,
    )
}

/// Le refus d'une feuille, rendu sous le type d'ES.
///
/// Les refus **explicites** de ferrite gardent le leur : ce sont des couts de
/// perimetre, et les confondre avec une erreur de requete les rendrait
/// invisibles a `perimetre.py` comme au fuzzer.
fn sur_le_shard(e: EsError) -> EsError {
    if e.ty == crate::error::UNSUPPORTED {
        return e;
    }
    // La phrase d'une date illisible est deja celle d'ES, **et son type
    // aussi** (`parse_exception`) : la reemballer changerait les deux.
    if e.reason.starts_with("failed to parse date field") {
        return e;
    }
    if e.valeur_illisible {
        return erreur_de_shard(format!("failed to create query: {}", e.reason));
    }
    erreur_de_shard(e.reason)
}

// ---------------------------------------------------------------------------
// `query_string`
// ---------------------------------------------------------------------------

const PERMIS_QS: &[&str] = &[
    "query",
    "default_field",
    "fields",
    "default_operator",
    "analyzer",
    "quote_analyzer",
    "allow_leading_wildcard",
    "analyze_wildcard",
    "enable_position_increments",
    "fuzziness",
    "fuzzy_prefix_length",
    "fuzzy_max_expansions",
    "fuzzy_transpositions",
    "fuzzy_rewrite",
    "phrase_slop",
    "boost",
    "auto_generate_synonyms_phrase_query",
    "lenient",
    "max_determinized_states",
    "minimum_should_match",
    "quote_field_suffix",
    "rewrite",
    "tie_breaker",
    "time_zone",
    "type",
    "escape",
];

/// La traduction de la clause en **Query DSL**, sans l'executer.
///
/// C'est le point d'entree que le surlignage emprunte : il lit la requete du
/// DSL, pas la requete tantivy, et une expression laissee opaque ne marquerait
/// rien la ou ES marque. Rendre du DSL plutot qu'une requete construite donne
/// aussi `explain` et `matched_queries` sans rien ecrire de plus.
pub fn query_string_en_dsl(body: &Value, ctx: &QueryCtx) -> EsResult<Value> {
    let o = body
        .as_object()
        .ok_or_else(|| EsError::parsing("[query_string] : un objet est attendu"))?;
    let expr = match o.get("query") {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => {
            return Err(EsError::parsing(
                "[query_string] must be provided with a [query]",
            ))
        }
    };
    let mut opts = lit_options(o, "query_string", PERMIS_QS)?;
    refuse_si_change(
        o,
        "query_string",
        "rewrite",
        &Value::Null,
        "il demande la forme **reecrite** de la requete Lucene, que ferrite n'a pas",
    )?;
    refuse_si_change(o, "query_string", "fuzzy_rewrite", &Value::Null, "le meme")?;
    refuse_si_change(
        o,
        "query_string",
        "max_determinized_states",
        &Value::Null,
        "il borne la taille de l'automate d'une `regexp` ; celui du crate `regex` ne se compte \
         pas en etats determinises, donc la borne ne voudrait pas dire la meme chose",
    )?;
    refuse_si_change(
        o,
        "query_string",
        "enable_position_increments",
        &json!(true),
        "a `false`, il **rapproche** les termes d'une phrase des qu'un mot vide a ete jete : \
         c'est un `slop` implicite, et ferrite refuse le `slop` (voir docs/compat.md)",
    )?;
    // `phrase_slop` est une valeur par defaut de `~n` : ferrite refuse le
    // `slop`, il refuse donc de le poser par defaut.
    refuse_si_change(
        o,
        "query_string",
        "phrase_slop",
        &json!(0),
        "la proximite d'une phrase demande le `slop` de [match_phrase], refuse dans tout ce \
         depot : tantivy et Lucene ne comptent pas les deplacements de la meme facon au-dela de \
         deux termes",
    )?;
    match o.get("type").and_then(Value::as_str) {
        None | Some("best_fields") => {}
        // Mesure : `most_fields` rend exactement les scores d'un `tie_breaker`
        // de 1.0 — les champs s'additionnent au lieu que le meilleur l'emporte.
        Some("most_fields") => opts.tie_breaker = 1.0,
        Some(t @ ("cross_fields" | "bool_prefix" | "phrase" | "phrase_prefix")) => {
            return Err(refuse_parametre(
                "query_string",
                "type",
                &format!(
                    "[{t}] : seuls [best_fields] et [most_fields] sont reproduits ; les autres \
                     changent la facon dont un terme devient une requete (statistiques fusionnees \
                     entre champs, phrase implicite, prefixe du dernier mot)"
                ),
            ))
        }
        Some(t) => {
            return Err(EsError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "parse_exception",
                format!("failed to parse [multi_match] query type [{t}]. unknown type."),
            ))
        }
    }
    // `escape: true` : ES echappe la chaine entiere avant de la lire, donc plus
    // rien n'y est un operateur.
    let expr = if lit_bool(o, "escape", false, "query_string")? {
        echappe(&expr)
    } else {
        expr
    };

    let arbre = analyse(&expr, opts.joker_en_tete)
        .map_err(|()| erreur_de_shard(format!("Failed to parse query [{expr}]")))?;
    // Une expression qui tient en un seul groupe de mots sur un seul champ :
    // la requete rendue est le booleen des termes analyses, et c'est **elle**
    // que `minimum_should_match` regle.
    let racine_est_un_groupe = matches!(
        &arbre,
        Some(Noeud::Feuille {
            champ,
            feuille: Feuille::Texte { phrase: false, .. }
        }) if t_un_seul_champ(ctx, &opts, champ.as_deref())
    );
    let t = Traducteur {
        ctx,
        opts: &opts,
        clause: "query_string",
        msm_de_la_racine: if racine_est_un_groupe {
            opts.msm.clone()
        } else {
            None
        },
    };
    let inner = match &arbre {
        // Une expression vide — ou dont aucun terme ne survit a l'analyse — ne
        // cherche rien, sans erreur (mesure).
        None => json!({"match_none": {}}),
        Some(n) => t
            .noeud(n, true)?
            .unwrap_or_else(|| json!({"match_none": {}})),
    };
    Ok(pose_le_boost(inner, opts.boost.as_ref()))
}

/// `escape` : la fonction `QueryParserBase.escape` d'ES, mot pour mot.
fn echappe(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '+'
                | '-'
                | '!'
                | '('
                | ')'
                | ':'
                | '^'
                | '['
                | ']'
                | '"'
                | '{'
                | '}'
                | '~'
                | '*'
                | '?'
                | '|'
                | '&'
                | '/'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Le `boost` de la clause entiere : il se pose sur la requete rendue.
fn pose_le_boost(q: Value, v: Option<&Value>) -> Value {
    match v {
        None | Some(Value::Null) => q,
        Some(b) => {
            // Un `bool` qui enveloppe : c'est ce que fait le DSL, et son
            // parseur validera la valeur.
            json!({"bool": {"must": [q], "boost": b}})
        }
    }
}

// ---------------------------------------------------------------------------
// 5. `simple_query_string`
// ---------------------------------------------------------------------------

/// Les drapeaux d'ES, avec leur valeur — la meme grammaire, ampute des
/// operateurs qu'on ne veut pas laisser ecrire.
fn drapeau(nom: &str) -> Option<u32> {
    Some(match nom {
        "NONE" => 0,
        "AND" => 1 << 0,
        "NOT" => 1 << 1,
        "OR" => 1 << 2,
        "PREFIX" => 1 << 3,
        "PHRASE" => 1 << 4,
        "PRECEDENCE" => 1 << 5,
        "ESCAPE" => 1 << 6,
        "WHITESPACE" => 1 << 7,
        "FUZZY" => 1 << 8,
        // `NEAR` et `SLOP` sont deux noms du meme drapeau.
        "NEAR" | "SLOP" => 1 << 9,
        "ALL" => u32::MAX,
        _ => return None,
    })
}

const F_AND: u32 = 1 << 0;
const F_NOT: u32 = 1 << 1;
const F_OR: u32 = 1 << 2;
const F_PREFIX: u32 = 1 << 3;
const F_PHRASE: u32 = 1 << 4;
const F_PRECEDENCE: u32 = 1 << 5;
const F_ESCAPE: u32 = 1 << 6;
const F_WHITESPACE: u32 = 1 << 7;
const F_FUZZY: u32 = 1 << 8;
const F_SLOP: u32 = 1 << 9;

const PERMIS_SQS: &[&str] = &[
    "query",
    "fields",
    "default_operator",
    "analyzer",
    "flags",
    "fuzzy_max_expansions",
    "fuzzy_prefix_length",
    "fuzzy_transpositions",
    "lenient",
    "analyze_wildcard",
    "minimum_should_match",
    "quote_field_suffix",
    "boost",
    "auto_generate_synonyms_phrase_query",
];

/// L'analyseur de `simple_query_string`, qui **ne leve jamais**.
///
/// Sa grammaire n'a pas de champs, pas de bornes et pas de regexp : `+` est le
/// ET, `|` le OU, `-` la negation, `"…"` une phrase, un `*` final un prefixe,
/// un `~n` final un flou (ou un `slop` derriere une phrase), et les parentheses
/// groupent. Tout le reste est du texte — c'est sa definition, et c'est ce que
/// mesure `diff_query_string.py` sur ses bords (`(`, `chat)`, `+`, `~`, `\`).
struct Simple<'a> {
    c: &'a [char],
    i: usize,
    flags: u32,
    op_et: bool,
}

impl Simple<'_> {
    fn actif(&self, f: u32) -> bool {
        self.flags & f != 0
    }

    fn separateur(&self, c: char) -> bool {
        (self.actif(F_WHITESPACE) && est_blanc(c))
            || (self.actif(F_AND) && c == '+')
            || (self.actif(F_OR) && c == '|')
            || (self.actif(F_PRECEDENCE) && matches!(c, '(' | ')'))
            || (self.actif(F_PHRASE) && c == '"')
    }

    fn sous_requete(&mut self) -> Option<Noeud> {
        let mut sommet: Option<Noeud> = None;
        // L'operateur courant est **consomme** a chaque fusion et revient au
        // defaut : `a + b c` vaut `(a ET b) OU c`, pas `a ET b ET c`.
        let mut et = self.op_et;
        // Les negations se **comptent** : `--chat` en annule une, `---chat`
        // n'en laisse qu'une (mesure contre ES 8.15).
        let mut nies = 0usize;
        while self.i < self.c.len() {
            let c = self.c[self.i];
            if self.actif(F_WHITESPACE) && est_blanc(c) {
                self.i += 1;
                continue;
            }
            if self.actif(F_AND) && c == '+' {
                et = true;
                self.i += 1;
                continue;
            }
            if self.actif(F_OR) && c == '|' {
                et = false;
                self.i += 1;
                continue;
            }
            if self.actif(F_NOT) && c == '-' {
                nies += 1;
                self.i += 1;
                continue;
            }
            if self.actif(F_PRECEDENCE) && c == ')' {
                self.i += 1;
                break;
            }
            let noeud = if self.actif(F_PRECEDENCE) && c == '(' {
                self.i += 1;
                self.sous_requete()
            } else if self.actif(F_PHRASE) && c == '"' {
                Some(self.phrase())
            } else {
                self.mot()
            };
            let Some(mut n) = noeud else {
                nies = 0;
                continue;
            };
            if nies % 2 == 1 {
                n = Noeud::Negation(Box::new(n));
            }
            nies = 0;
            // Un operateur pose **avant** la premiere clause ne relie rien :
            // `+chat -chien` rend l'union de `chat` et de « pas chien », pas
            // leur intersection (mesure contre ES 8.15).
            sommet = Some(match sommet {
                None => n,
                Some(s) => fusionne(s, n, et),
            });
            et = self.op_et;
        }
        sommet
    }

    fn phrase(&mut self) -> Noeud {
        self.i += 1;
        let mut texte = String::new();
        while self.i < self.c.len() {
            let c = self.c[self.i];
            if c == '"' {
                self.i += 1;
                break;
            }
            if self.actif(F_ESCAPE) && c == '\\' && self.i + 1 < self.c.len() {
                texte.push(self.c[self.i + 1]);
                self.i += 2;
                continue;
            }
            texte.push(c);
            self.i += 1;
        }
        let slop = self.suffixe_tilde().unwrap_or(0);
        Noeud::Feuille {
            champ: None,
            feuille: Feuille::Texte {
                texte,
                phrase: true,
                slop: if self.actif(F_SLOP) { slop } else { 0 },
            },
        }
    }

    /// Le `~…` colle a ce qui precede : il court jusqu'au bout du mot, et ce
    /// qui le suit est lu comme un **entier**.
    ///
    /// Trois regles, toutes mesurees contre ES 8.15 et dont aucune ne se
    /// devine : `~` nu vaut la distance **2** (et non `AUTO` — `chot~` rend un
    /// document de plus que `chot~1`), un nombre est ramene dans `[0, 2]`
    /// (`chat~-1` cherche `chat` exactement), et ce qui n'est pas un nombre
    /// vaut **0**, donc un terme exact : `chot~x` ne rend aucun document la ou
    /// `chot~` en rend cinq.
    fn suffixe_tilde(&mut self) -> Option<u32> {
        if self.c.get(self.i) != Some(&'~') {
            return None;
        }
        let mut j = self.i + 1;
        while j < self.c.len() && !self.separateur(self.c[j]) {
            j += 1;
        }
        let brut: String = self.c[self.i + 1..j].iter().collect();
        self.i = j;
        if brut.is_empty() {
            return Some(2);
        }
        Some(brut.parse::<i64>().map_or(0, |n| n.clamp(0, 2) as u32))
    }

    fn mot(&mut self) -> Option<Noeud> {
        let mut texte = String::new();
        // Le `*` final ne fait un prefixe que s'il n'est pas **echappe** :
        // `chien\*` cherche le terme `chien*` (mesure contre ES 8.15).
        let mut etoile_echappee = false;
        while self.i < self.c.len() {
            let c = self.c[self.i];
            if self.separateur(c) {
                break;
            }
            if self.actif(F_ESCAPE) && c == '\\' {
                // Un `\` en toute fin d'entree n'echappe rien : ES le laisse
                // tomber, il n'entre pas dans le terme (mesure — invisible sur
                // un `text`, dont l'analyzer l'enleve, decisif sur un `keyword`).
                if self.i + 1 >= self.c.len() {
                    self.i += 1;
                    break;
                }
                etoile_echappee = self.c[self.i + 1] == '*';
                texte.push(self.c[self.i + 1]);
                self.i += 2;
                continue;
            }
            if c == '~' && (self.actif(F_FUZZY) || self.actif(F_SLOP)) {
                break;
            }
            etoile_echappee = false;
            texte.push(c);
            self.i += 1;
        }
        let flou = self.suffixe_tilde().filter(|_| self.actif(F_FUZZY));
        if texte.is_empty() {
            return None;
        }
        // Un `*` final fait un prefixe ; ailleurs c'est un caractere ordinaire
        // (mesure : `*chat` rend les memes documents que `chat`, `ch?t` aucun).
        if self.actif(F_PREFIX) && !etoile_echappee && texte.len() > 1 && texte.ends_with('*') {
            return Some(Noeud::Feuille {
                champ: None,
                feuille: Feuille::Prefixe(texte[..texte.len() - 1].to_string()),
            });
        }
        Some(Noeud::Feuille {
            champ: None,
            feuille: match flou {
                // ES borne la distance au lieu de refuser : c'est la
                // difference de fond avec `query_string`, qui rend 400.
                Some(n) => Feuille::Flou {
                    texte,
                    distance: Some(n.to_string()),
                },
                None => Feuille::Texte {
                    texte,
                    phrase: false,
                    slop: 0,
                },
            },
        })
    }
}

pub fn simple_query_string_en_dsl(body: &Value, ctx: &QueryCtx) -> EsResult<Value> {
    let o = body
        .as_object()
        .ok_or_else(|| EsError::parsing("[simple_query_string] : un objet est attendu"))?;
    let expr = match o.get("query") {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => return Err(EsError::parsing("[simple_query_string] query text missing")),
    };
    let mut opts = lit_options(o, "simple_query_string", PERMIS_SQS)?;
    // Les champs se combinent en **somme**, pas au meilleur : mesure contre
    // ES 8.15, ou le `DisjunctionMaxQuery` de `simple_query_string` porte un
    // `tie_breaker` de 1.0 la ou celui de `query_string` vaut 0.
    opts.tie_breaker = 1.0;
    let mut flags = u32::MAX;
    if let Some(v) = o.get("flags") {
        let brut = v.as_str().map_or_else(|| v.to_string(), str::to_string);
        flags = 0;
        for nom in brut.split('|') {
            let f = drapeau(nom).ok_or_else(|| {
                EsError::illegal_argument(format!("Unknown simple_query_string flag [{brut}]"))
            })?;
            flags |= f;
        }
    }
    if opts.analyse_jokers {
        // ES le sert ; ferrite ne l'expose que sur `query_string`, ou il est
        // mesure. Le refus est nomme plutot qu'ignore.
        return Err(refuse_parametre(
            "simple_query_string",
            "analyze_wildcard",
            "il n'est mesure que sur [query_string]",
        ));
    }

    // `*` seul : tous les documents, avant meme d'analyser (mesure).
    let inner = if expr.trim() == "*" {
        json!({"match_all": {}})
    } else {
        let c: Vec<char> = expr.chars().collect();
        let mut s = Simple {
            c: &c,
            i: 0,
            flags,
            op_et: opts.op_et,
        };
        match s.sous_requete() {
            None => json!({"match_none": {}}),
            Some(arbre) => {
                let t = Traducteur {
                    ctx,
                    opts: &opts,
                    clause: "simple_query_string",
                    msm_de_la_racine: None,
                };
                t.noeud(&arbre, true)?
                    .unwrap_or_else(|| json!({"match_none": {}}))
            }
        }
    };
    Ok(pose_le_boost(inner, opts.boost.as_ref()))
}

/// `{"query_string": {…}}` — la clause, executee.
pub fn query_string(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    dsl::build_query(&query_string_en_dsl(body, ctx)?, ctx)
}

/// `{"simple_query_string": {…}}` — la clause, executee.
pub fn simple_query_string(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    dsl::build_query(&simple_query_string_en_dsl(body, ctx)?, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jetons(s: &str) -> Vec<Jeton> {
        lex(s).expect("lexable")
    }

    #[test]
    fn le_plus_long_match_departage_les_jetons() {
        // `AND` est un operateur, `ANDx` un terme, `and` un terme.
        assert_eq!(jetons("AND"), vec![Jeton::And]);
        assert_eq!(jetons("ANDx"), vec![Jeton::Term("ANDx".into())]);
        assert_eq!(jetons("and"), vec![Jeton::Term("and".into())]);
        // `*` seul est l'etoile, `cha*` un prefixe, `ch?t` un joker.
        assert_eq!(jetons("*"), vec![Jeton::Star]);
        assert_eq!(jetons("cha*"), vec![Jeton::PrefixTerm("cha*".into())]);
        assert_eq!(jetons("ch?t"), vec![Jeton::WildTerm("ch?t".into())]);
        // `-` en tete est un operateur, au milieu d'un mot un caractere.
        assert_eq!(jetons("chat-huant"), vec![Jeton::Term("chat-huant".into())]);
    }

    #[test]
    fn le_boost_n_accepte_qu_un_nombre() {
        assert!(lex("a^x").is_err());
        assert!(lex("a^").is_err());
        assert!(lex("a^-1").is_err());
        assert_eq!(
            jetons("a^2"),
            vec![Jeton::Term("a".into()), Jeton::Carat(2.0)]
        );
    }

    #[test]
    fn la_grammaire_refuse_ce_qu_es_refuse() {
        for expr in [
            "titre:", ":", "AND chat", "chat AND", "NOT", "(chat", "chat)", "()", "a b)c", "+",
            "-", "+-chat", "n:[1 TO", "^2", "~",
        ] {
            assert!(
                analyse(expr, true).is_err(),
                "[{expr}] devrait etre une erreur de syntaxe"
            );
        }
        for expr in [
            "chat",
            "chat chien",
            "titre:chat",
            "n:[1 TO 10]",
            "titre:{a TO b]",
            "*:*",
            "chat~",
            "(a OR b)^3",
            "\"le chat\"",
            "\\AND",
            "((((chat))))",
            "chat~1.5",
        ] {
            assert!(analyse(expr, true).is_ok(), "[{expr}] devrait etre lisible");
        }
    }

    #[test]
    fn le_joker_en_tete_se_refuse_a_la_syntaxe() {
        assert!(analyse("*hat", true).is_ok());
        assert!(analyse("*hat", false).is_err());
        assert!(analyse("?hat", false).is_err());
        assert!(analyse("cha*", false).is_ok());
    }
}
