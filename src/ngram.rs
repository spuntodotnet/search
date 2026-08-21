//! Les n-grammes : le tokenizer et le filtre `ngram` / `edge_ngram`.
//!
//! C'est la brique de l'autocompletion « au fil de la frappe » : elle travaille
//! a **l'indexation** (chaque valeur est decoupee en prefixes, ou en fenetres
//! glissantes), la ou `match_phrase_prefix` travaille a la requete. Un CMS qui
//! propose des pages pendant qu'on tape n'a pas d'autre moyen.
//!
//! Tout ce qui suit est **mesure** contre un Elasticsearch 8.15.0 par
//! `tests/compat/diff_analyzers.py`, jamais deduit de la documentation. Les
//! bords qui ne l'etaient pas :
//!
//! * l'ordre d'emission est **par position de depart**, longueurs croissantes
//!   (`abcdef` en 3-15 donne `abc abcd abcde abcdef bcd bcde…`), et pas
//!   l'inverse ;
//! * le **tokenizer** avance d'une position par gramme, le **filtre** pose tous
//!   les grammes d'un token **a la position de ce token** — un n-gramme mal
//!   positionne casse `match_phrase` sans changer le compte de tokens ;
//! * `index.max_ngram_diff` (defaut **1**) ne borne que `ngram`, jamais
//!   `edge_ngram` — et il est verifie **avant** que `min_gram >= 1` ;
//! * un token plus court que `min_gram` est **jete** par le filtre (il ne
//!   ressort pas tel quel), sauf `preserve_original` ;
//! * les grammes se comptent en **points de code** (un emoji est un gramme),
//!   mais les offsets rendus par `_analyze` se comptent en unites UTF-16 —
//!   c'est la convention de Java, et [`crate::api::indices`] fait la
//!   conversion ;
//! * `side` n'existe **que** sur le filtre `edge_ngram` ; sur le tokenizer, ES
//!   l'ignore en silence (Wagtail en ecrit un), donc ferrite l'ignore aussi.

use std::collections::VecDeque;

use serde_json::{json, Map, Value};
use tantivy::tokenizer::{Token, TokenFilter, TokenStream, Tokenizer};

use crate::error::{EsError, EsResult};

/// Le defaut d'ES pour `index.max_ngram_diff`.
pub const MAX_NGRAM_DIFF_DEFAUT: i64 = 1;

/// Les bornes d'un n-gramme, telles qu'ES les lit et les valide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bornes {
    pub min: usize,
    pub max: usize,
}

impl Bornes {
    /// Lit `min_gram` / `max_gram` et applique les trois controles d'ES, **dans
    /// son ordre** : l'ecart d'abord (c'est pourquoi `min_gram: -1` sort une
    /// erreur d'ecart et non de borne), puis le zero, puis min <= max.
    ///
    /// `borne_ecart` est `None` pour `edge_ngram` : `index.max_ngram_diff` ne
    /// le concerne pas (mesure : `edge_ngram` 1-15 passe sur un index qui n'a
    /// pas touche au reglage).
    fn lire(obj: &Map<String, Value>, ou: &str, borne_ecart: Option<i64>) -> EsResult<Self> {
        let min = lire_entier(obj, "min_gram", 1, ou)?;
        let max = lire_entier(obj, "max_gram", 2, ou)?;
        if let Some(limite) = borne_ecart {
            let ecart = max - min;
            if ecart > limite {
                return Err(EsError::illegal_argument(format!(
                    "The difference between max_gram and min_gram in NGram Tokenizer must be less \
                     than or equal to: [{limite}] but was [{ecart}]. This limit can be set by \
                     changing the [index.max_ngram_diff] index level setting."
                )));
            }
        }
        if min < 1 {
            return Err(EsError::illegal_argument("minGram must be greater than zero"));
        }
        if min > max {
            return Err(EsError::illegal_argument(
                "minGram must not be greater than maxGram",
            ));
        }
        Ok(Self {
            min: min as usize,
            max: max as usize,
        })
    }
}

/// Le cote d'un `edge_ngram` : `front` (defaut) ou `back`.
///
/// Deprecie chez ES depuis la 8.x, mais toujours servi — et un mapping venu
/// d'une instance plus ancienne en porte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Cote {
    #[default]
    Front,
    Back,
}

/// Les classes de caracteres de `token_chars`.
///
/// Sans elles, le tokenizer ne coupe **nulle part** : le texte entier est un
/// seul mot, espaces et ponctuation compris (mesure : `ngram` par defaut sur
/// « Quick Fox » rend `k `, ` `, ` F`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classe {
    Lettre,
    Chiffre,
    Espace,
    Ponctuation,
    Symbole,
    Custom,
}

impl Classe {
    fn parse(nom: &str) -> Option<Self> {
        Some(match nom {
            "letter" => Self::Lettre,
            "digit" => Self::Chiffre,
            "whitespace" => Self::Espace,
            "punctuation" => Self::Ponctuation,
            "symbol" => Self::Symbole,
            "custom" => Self::Custom,
            _ => return None,
        })
    }

    fn nom(self) -> &'static str {
        match self {
            Self::Lettre => "letter",
            Self::Chiffre => "digit",
            Self::Espace => "whitespace",
            Self::Ponctuation => "punctuation",
            Self::Symbole => "symbol",
            Self::Custom => "custom",
        }
    }
}

/// Ce qui fait partie d'un mot, pour un tokenizer a n-grammes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TokenChars {
    classes: Vec<Classe>,
    custom: String,
}

impl TokenChars {
    /// Vide = aucune coupe, le texte entier est un mot (le defaut d'ES).
    fn est_vide(&self) -> bool {
        self.classes.is_empty()
    }

    fn contient(&self, c: char) -> bool {
        self.classes.iter().any(|k| match k {
            Classe::Lettre => est_lettre(c),
            Classe::Chiffre => est_chiffre(c),
            Classe::Espace => est_espace(c),
            Classe::Ponctuation => est_ponctuation(c),
            Classe::Symbole => est_symbole(c),
            Classe::Custom => self.custom.contains(c),
        })
    }

    fn lire(obj: &Map<String, Value>, ou: &str) -> EsResult<Self> {
        let mut classes = Vec::new();
        if let Some(v) = obj.get("token_chars") {
            let liste = v.as_array().ok_or_else(|| {
                EsError::illegal_argument(format!("[{ou}.token_chars] : une liste est attendue"))
            })?;
            for x in liste {
                let nom = x.as_str().ok_or_else(|| {
                    EsError::illegal_argument(format!("[{ou}.token_chars] : chaines attendues"))
                })?;
                let classe = Classe::parse(nom).ok_or_else(|| {
                    EsError::illegal_argument(format!(
                        "Unknown token type: '{nom}', must be one of [letter, digit, whitespace, \
                         punctuation, symbol, custom] (ferrite ne sert pas les noms de categories \
                         Unicode qu'ES accepte en plus, voir docs/compat.md)"
                    ))
                })?;
                if !classes.contains(&classe) {
                    classes.push(classe);
                }
            }
        }
        let custom = match obj.get("custom_token_chars") {
            Some(Value::String(s)) => s.clone(),
            None => String::new(),
            Some(autre) => {
                return Err(EsError::illegal_argument(format!(
                    "[{ou}.custom_token_chars] : une chaine est attendue, recu [{autre}]"
                )))
            }
        };
        if classes.contains(&Classe::Custom) && custom.is_empty() {
            return Err(EsError::illegal_argument(
                "Token type: 'custom' requires setting `custom_token_chars`",
            ));
        }
        Ok(Self { classes, custom })
    }

    fn to_json(&self) -> (Value, Option<Value>) {
        let noms: Vec<Value> = self.classes.iter().map(|c| json!(c.nom())).collect();
        let custom = (!self.custom.is_empty()).then(|| json!(self.custom));
        (Value::Array(noms), custom)
    }
}

// ---------------------------------------------------------------------------
// Le tokenizer
// ---------------------------------------------------------------------------

/// `ngram` et `edge_ngram` cote **tokenizer** : ils decoupent le texte, donc ils
/// avancent d'une position par gramme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NgramTokenizer {
    pub bornes: Bornes,
    pub chars: TokenChars,
    /// `true` pour `edge_ngram` : seuls les grammes qui partent du debut du mot.
    pub edge: bool,
}

impl NgramTokenizer {
    /// `ngram` / `edge_ngram` cites par leur nom, sans bornes : les defauts
    /// d'ES (1 et 2, aucun `token_chars`).
    pub fn defaut(edge: bool) -> Self {
        Self {
            bornes: Bornes { min: 1, max: 2 },
            chars: TokenChars::default(),
            edge,
        }
    }

    /// Lit une declaration `{"type": "ngram", …}` de `analysis.tokenizer`.
    pub fn parse(
        obj: &Map<String, Value>,
        edge: bool,
        ou: &str,
        max_ngram_diff: i64,
    ) -> EsResult<Self> {
        let bornes = Bornes::lire(obj, ou, (!edge).then_some(max_ngram_diff))?;
        expect_only(
            obj,
            &[
                "type",
                "min_gram",
                "max_gram",
                "token_chars",
                "custom_token_chars",
                // ES ne lit pas `side` sur un tokenizer : il l'ignore et rend
                // les grammes de tete. Le refuser ferait echouer un mapping
                // qu'un vrai ES accepte (Wagtail en ecrit un) ; l'accepter
                // rend exactement ce qu'ES rend.
                "side",
            ],
            ou,
        )?;
        Ok(Self {
            bornes,
            chars: TokenChars::lire(obj, ou)?,
            edge,
        })
    }

    pub fn to_json(&self) -> Value {
        let (classes, custom) = self.chars.to_json();
        let mut o = json!({
            "type": if self.edge { "edge_ngram" } else { "ngram" },
            "min_gram": self.bornes.min,
            "max_gram": self.bornes.max,
            "token_chars": classes,
        });
        if let (Some(c), Some(m)) = (custom, o.as_object_mut()) {
            m.insert("custom_token_chars".into(), c);
        }
        o
    }

    /// Les mots sur lesquels les grammes se calculent : le texte entier si
    /// `token_chars` est vide, sinon chaque suite maximale de caracteres
    /// retenus.
    fn mots(&self, texte: &str) -> Vec<(usize, usize)> {
        if self.chars.est_vide() {
            return if texte.is_empty() {
                Vec::new()
            } else {
                vec![(0, texte.len())]
            };
        }
        let mut out = Vec::new();
        let mut debut: Option<usize> = None;
        for (i, c) in texte.char_indices() {
            if self.chars.contient(c) {
                debut.get_or_insert(i);
            } else if let Some(d) = debut.take() {
                out.push((d, i));
            }
        }
        if let Some(d) = debut {
            out.push((d, texte.len()));
        }
        out
    }
}

impl Tokenizer for NgramTokenizer {
    type TokenStream<'a> = crate::analysis::VecTokenStream;

    fn token_stream<'a>(&'a mut self, texte: &'a str) -> Self::TokenStream<'a> {
        let mut tokens = Vec::new();
        let mut position = 0usize;
        for (debut, fin) in self.mots(texte) {
            let mot = &texte[debut..fin];
            // Les grammes se comptent en points de code, pas en octets.
            let bornes_car: Vec<usize> = mot
                .char_indices()
                .map(|(i, _)| i)
                .chain(std::iter::once(mot.len()))
                .collect();
            let n = bornes_car.len() - 1;
            let departs = if self.edge { 1 } else { n };
            for depart in 0..departs {
                for taille in self.bornes.min..=self.bornes.max {
                    if depart + taille > n {
                        break;
                    }
                    let (a, b) = (bornes_car[depart], bornes_car[depart + taille]);
                    tokens.push(Token {
                        offset_from: debut + a,
                        offset_to: debut + b,
                        position,
                        text: mot[a..b].to_string(),
                        position_length: 1,
                    });
                    position += 1;
                }
            }
        }
        crate::analysis::VecTokenStream::new(tokens)
    }
}

// ---------------------------------------------------------------------------
// Le filtre
// ---------------------------------------------------------------------------

/// `ngram` et `edge_ngram` cote **filtre** : ils remplacent chaque token par ses
/// grammes, **a la position du token d'origine** et avec ses offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NgramFilter {
    pub bornes: Bornes,
    pub edge: bool,
    pub cote: Cote,
    pub preserve: bool,
}

impl NgramFilter {
    /// `ngram` / `edge_ngram` cites par leur nom : les defauts d'ES — qui ne
    /// sont **pas** ceux d'une declaration.
    ///
    /// Mesure : `filter: ["edge_ngram"]` sur `Quick` rend `[Q]`, la ou
    /// `filter: [{"type": "edge_ngram"}]` rend `[Q, Qu]`. Le filtre pre-bati
    /// porte le defaut de Lucene (`max_gram` = 1), celui qu'on configure porte
    /// celui d'Elasticsearch (2).
    pub fn defaut(edge: bool) -> Self {
        Self {
            bornes: Bornes {
                min: 1,
                max: if edge { 1 } else { 2 },
            },
            edge,
            cote: Cote::Front,
            preserve: false,
        }
    }

    pub fn parse(
        obj: &Map<String, Value>,
        edge: bool,
        ou: &str,
        max_ngram_diff: i64,
    ) -> EsResult<Self> {
        let bornes = Bornes::lire(obj, ou, (!edge).then_some(max_ngram_diff))?;
        let mut permis: Vec<&str> = vec!["type", "min_gram", "max_gram", "preserve_original"];
        if edge {
            permis.push("side");
        }
        expect_only(obj, &permis, ou)?;
        let cote = match obj.get("side") {
            None => Cote::Front,
            Some(Value::String(s)) if s == "front" => Cote::Front,
            Some(Value::String(s)) if s == "back" => Cote::Back,
            Some(v) => {
                let brut = v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string());
                return Err(EsError::illegal_argument(format!("invalid side: {brut}")));
            }
        };
        Ok(Self {
            bornes,
            edge,
            cote,
            preserve: lire_booleen(obj, "preserve_original", ou)?,
        })
    }

    pub fn to_json(&self) -> Value {
        let mut o = json!({
            "type": if self.edge { "edge_ngram" } else { "ngram" },
            "min_gram": self.bornes.min,
            "max_gram": self.bornes.max,
            "preserve_original": self.preserve,
        });
        if self.edge {
            if let Some(m) = o.as_object_mut() {
                m.insert(
                    "side".into(),
                    json!(match self.cote {
                        Cote::Front => "front",
                        Cote::Back => "back",
                    }),
                );
            }
        }
        o
    }

    /// Les grammes d'un token, dans l'ordre ou ES les emet.
    fn grammes(&self, source: &Token, sortie: &mut VecDeque<Token>) {
        let bornes_car: Vec<usize> = source
            .text
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(source.text.len()))
            .collect();
        let n = bornes_car.len() - 1;
        let mut pousse = |a: usize, b: usize| {
            sortie.push_back(Token {
                text: source.text[a..b].to_string(),
                ..source.clone()
            });
        };
        // Un token trop court ne ressort pas — sauf `preserve_original`, qui le
        // rend tel quel (mesure : `ab` sous un `ngram` 3-3 disparait).
        if n < self.bornes.min {
            if self.preserve {
                pousse(0, source.text.len());
            }
            return;
        }
        if self.edge {
            for taille in self.bornes.min..=self.bornes.max.min(n) {
                match self.cote {
                    Cote::Front => pousse(0, bornes_car[taille]),
                    Cote::Back => pousse(bornes_car[n - taille], source.text.len()),
                }
            }
        } else {
            for depart in 0..n {
                for taille in self.bornes.min..=self.bornes.max {
                    if depart + taille > n {
                        break;
                    }
                    pousse(bornes_car[depart], bornes_car[depart + taille]);
                }
            }
        }
        // L'original n'est ajoute que s'il n'a pas deja ete emis.
        if self.preserve && n > self.bornes.max {
            pousse(0, source.text.len());
        }
    }
}

impl TokenFilter for NgramFilter {
    type Tokenizer<T: Tokenizer> = NgramFilterWrapper<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> NgramFilterWrapper<T> {
        NgramFilterWrapper {
            conf: self,
            inner: tokenizer,
        }
    }
}

#[derive(Clone)]
pub struct NgramFilterWrapper<T> {
    conf: NgramFilter,
    inner: T,
}

impl<T: Tokenizer> Tokenizer for NgramFilterWrapper<T> {
    type TokenStream<'a> = NgramFilterStream<T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, texte: &'a str) -> Self::TokenStream<'a> {
        NgramFilterStream {
            conf: self.conf.clone(),
            tail: self.inner.token_stream(texte),
            file: VecDeque::new(),
            courant: Token::default(),
        }
    }
}

pub struct NgramFilterStream<T> {
    conf: NgramFilter,
    tail: T,
    file: VecDeque<Token>,
    courant: Token,
}

impl<T: TokenStream> TokenStream for NgramFilterStream<T> {
    fn advance(&mut self) -> bool {
        loop {
            if let Some(t) = self.file.pop_front() {
                self.courant = t;
                return true;
            }
            if !self.tail.advance() {
                return false;
            }
            let source = self.tail.token().clone();
            self.conf.grammes(&source, &mut self.file);
        }
    }

    fn token(&self) -> &Token {
        &self.courant
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.courant
    }
}

// ---------------------------------------------------------------------------
// Lecture des parametres
// ---------------------------------------------------------------------------

/// Refuse toute cle non prevue. ES, lui, **ignore** ce qu'il ne connait pas
/// dans une declaration d'analyse ; ferrite le refuse, comme il refuse deja une
/// cle inconnue dans un analyzer — un parametre avale en silence changerait ce
/// qui entre dans l'index sans que personne le sache.
fn expect_only(obj: &Map<String, Value>, permis: &[&str], ou: &str) -> EsResult<()> {
    for cle in obj.keys() {
        if !permis.contains(&cle.as_str()) {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [{cle}] dans [{ou}] ; parametres acceptes : {}",
                permis.join(", ")
            )));
        }
    }
    Ok(())
}

/// ES accepte un entier comme sa forme chaine (`"3"`) : les reglages d'index
/// ressortent en chaines de chez lui, donc un client qui les recopie en envoie.
fn lire_entier(obj: &Map<String, Value>, cle: &str, defaut: i64, ou: &str) -> EsResult<i64> {
    match obj.get(cle) {
        None | Some(Value::Null) => Ok(defaut),
        Some(Value::Number(n)) if n.is_i64() => Ok(n.as_i64().unwrap_or(defaut)),
        Some(Value::String(s)) => s.trim().parse::<i64>().map_err(|_| {
            EsError::illegal_argument(format!(
                "Failed to parse value [{s}] for setting [{ou}.{cle}]"
            ))
        }),
        Some(autre) => Err(EsError::illegal_argument(format!(
            "Failed to parse value [{autre}] for setting [{ou}.{cle}]"
        ))),
    }
}

fn lire_booleen(obj: &Map<String, Value>, cle: &str, ou: &str) -> EsResult<bool> {
    match obj.get(cle) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(Value::String(s)) if s == "true" => Ok(true),
        Some(Value::String(s)) if s == "false" => Ok(false),
        Some(autre) => Err(EsError::illegal_argument(format!(
            "Failed to parse value [{autre}] as only [true] or [false] are allowed for setting \
             [{ou}.{cle}]"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Les classes de caracteres, telles que Java les definit
// ---------------------------------------------------------------------------
//
// `token_chars` nomme des categories generales d'Unicode, et Lucene les lit
// avec `Character.getType`. Les predicats de la bibliotheque standard de Rust
// n'y correspondent pas : `is_alphabetic` accepte `Ⅰ` (Nl) et les signes
// vocaliques indiens (Other_Alphabetic) qu'`isLetter` refuse, `is_numeric`
// accepte `½` et `①` (No) qu'`isDigit` refuse. Les tables ci-dessous sont donc
// generees depuis les categories generales d'Unicode 15.1 :
//
//     python3 -c "import unicodedata; ..."   (recette dans docs/compat.md)
//
// et **mesurees** contre un vrai ES par `diff_analyzers.py --classes`, qui
// demande sa classe a chaque caractere d'un echantillon des deux cotes.

fn dans(table: &[(u32, u32)], c: char) -> bool {
    let c = c as u32;
    table
        .binary_search_by(|&(a, b)| {
            if c < a {
                std::cmp::Ordering::Greater
            } else if c > b {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

fn est_lettre(c: char) -> bool {
    dans(LETTRE, c)
}

fn est_chiffre(c: char) -> bool {
    dans(CHIFFRE, c)
}

fn est_ponctuation(c: char) -> bool {
    dans(PONCTUATION, c)
}

fn est_symbole(c: char) -> bool {
    dans(SYMBOLE, c)
}

/// `Character.isWhitespace` — qui n'est **pas** la propriete `White_Space`
/// d'Unicode : l'espace insecable (U+00A0, U+2007, U+202F) en est exclu, la ou
/// `char::is_whitespace` de Rust l'inclut.
fn est_espace(c: char) -> bool {
    if matches!(c, '\u{a0}' | '\u{2007}' | '\u{202f}') {
        return false;
    }
    matches!(c, '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | '\u{1c}'..='\u{1f}') || c.is_whitespace()
}

include!("unicode_classes.rs");

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(edge: bool, min: usize, max: usize, classes: &[Classe], texte: &str) -> Vec<String> {
        let mut t = NgramTokenizer {
            bornes: Bornes { min, max },
            chars: TokenChars {
                classes: classes.to_vec(),
                custom: String::new(),
            },
            edge,
        };
        let mut flux = t.token_stream(texte);
        let mut out = Vec::new();
        while flux.advance() {
            out.push(flux.token().text.clone());
        }
        out
    }

    /// L'ordre mesure chez ES : par position de depart, longueurs croissantes.
    #[test]
    fn ordre_des_grammes() {
        assert_eq!(
            tok(false, 3, 15, &[], "abcdef"),
            [
                "abc", "abcd", "abcde", "abcdef", "bcd", "bcde", "bcdef", "cde", "cdef", "def"
            ]
        );
    }

    /// Sans `token_chars`, rien n'est un separateur — pas meme l'espace.
    #[test]
    fn sans_token_chars_le_texte_entier_est_un_mot() {
        assert_eq!(
            tok(false, 1, 2, &[], "ab c"),
            ["a", "ab", "b", "b ", " ", " c", "c"]
        );
    }

    #[test]
    fn token_chars_coupe_les_mots() {
        assert_eq!(
            tok(true, 1, 2, &[Classe::Lettre], "ab1 cd"),
            ["a", "ab", "c", "cd"]
        );
    }

    /// Les grammes se comptent en points de code : un emoji est un gramme.
    #[test]
    fn les_grammes_comptent_des_points_de_code() {
        assert_eq!(tok(false, 1, 1, &[], "a\u{1f600}b"), ["a", "\u{1f600}", "b"]);
    }

    /// `Ⅰ` (Nl) n'est pas une lettre chez Java, `½` (No) n'est pas un chiffre :
    /// les predicats de Rust disent l'inverse, d'ou les tables.
    #[test]
    fn les_classes_sont_celles_de_java_pas_celles_de_rust() {
        assert!(est_lettre('e') && est_lettre('é') && est_lettre('漢'));
        assert!(!est_lettre('Ⅰ'), "Nl n'est pas une lettre");
        assert!(est_chiffre('3') && est_chiffre('٣'));
        assert!(!est_chiffre('½') && !est_chiffre('①'), "No n'est pas Nd");
        assert!(est_ponctuation('-') && est_ponctuation('_') && est_ponctuation('«'));
        assert!(est_symbole('+') && est_symbole('€') && est_symbole('©'));
        assert!(est_espace(' ') && est_espace('\t') && est_espace('\u{2028}'));
        assert!(!est_espace('\u{a0}'), "l'insecable n'est pas un espace Java");
    }

    fn filtre(f: &NgramFilter, mots: &[&str]) -> Vec<(String, usize)> {
        let mut out = Vec::new();
        for (i, m) in mots.iter().enumerate() {
            let source = Token {
                offset_from: 0,
                offset_to: m.len(),
                position: i,
                text: (*m).to_string(),
                position_length: 1,
            };
            let mut file = VecDeque::new();
            f.grammes(&source, &mut file);
            out.extend(file.into_iter().map(|t| (t.text, t.position)));
        }
        out
    }

    /// Le filtre garde la position du token source : c'est ce qui fait qu'un
    /// `match_phrase` continue de marcher.
    #[test]
    fn le_filtre_pose_les_grammes_a_la_position_du_token() {
        let f = NgramFilter {
            bornes: Bornes { min: 1, max: 3 },
            edge: true,
            cote: Cote::Front,
            preserve: false,
        };
        assert_eq!(
            filtre(&f, &["abcd", "ef"]),
            [
                ("a".into(), 0),
                ("ab".into(), 0),
                ("abc".into(), 0),
                ("e".into(), 1),
                ("ef".into(), 1)
            ]
        );
    }

    #[test]
    fn edge_ngram_par_la_fin() {
        let f = NgramFilter {
            bornes: Bornes { min: 1, max: 3 },
            edge: true,
            cote: Cote::Back,
            preserve: false,
        };
        assert_eq!(
            filtre(&f, &["abcd"]),
            [("d".into(), 0), ("cd".into(), 0), ("bcd".into(), 0)]
        );
    }

    /// Un token plus court que `min_gram` disparait — sauf `preserve_original`.
    #[test]
    fn token_trop_court() {
        let mut f = NgramFilter {
            bornes: Bornes { min: 3, max: 3 },
            edge: false,
            cote: Cote::Front,
            preserve: false,
        };
        assert_eq!(filtre(&f, &["ab", "abcd"]), [("abc".into(), 1), ("bcd".into(), 1)]);
        f.preserve = true;
        assert_eq!(
            filtre(&f, &["ab", "abcd"]),
            [
                ("ab".into(), 0),
                ("abc".into(), 1),
                ("bcd".into(), 1),
                ("abcd".into(), 1)
            ]
        );
    }

    /// `preserve_original` n'ajoute pas un doublon quand le token a deja ete
    /// emis tel quel.
    #[test]
    fn preserve_original_ne_double_pas() {
        let f = NgramFilter {
            bornes: Bornes { min: 1, max: 2 },
            edge: false,
            cote: Cote::Front,
            preserve: true,
        };
        assert_eq!(
            filtre(&f, &["ab"]),
            [("a".into(), 0), ("ab".into(), 0), ("b".into(), 0)]
        );
    }

    #[test]
    fn les_trois_controles_d_es_dans_son_ordre() {
        let o = |v: Value| v.as_object().unwrap().clone();
        // L'ecart passe avant tout : `min_gram: -1` sort une erreur d'ecart.
        let e = Bornes::lire(&o(json!({"min_gram": -1, "max_gram": 1})), "t", Some(1)).unwrap_err();
        assert!(e.reason.contains("difference between max_gram"), "{e:?}");
        let e = Bornes::lire(&o(json!({"min_gram": 0, "max_gram": 1})), "t", Some(1)).unwrap_err();
        assert_eq!(e.reason, "minGram must be greater than zero");
        let e = Bornes::lire(&o(json!({"min_gram": 3, "max_gram": 2})), "t", Some(1)).unwrap_err();
        assert_eq!(e.reason, "minGram must not be greater than maxGram");
        // `edge_ngram` n'est pas borne par `max_ngram_diff`.
        assert!(Bornes::lire(&o(json!({"min_gram": 1, "max_gram": 15})), "t", None).is_ok());
        assert!(Bornes::lire(&o(json!({"min_gram": 1, "max_gram": 15})), "t", Some(1)).is_err());
        assert!(Bornes::lire(&o(json!({"min_gram": "1", "max_gram": "15"})), "t", Some(14)).is_ok());
    }
}
