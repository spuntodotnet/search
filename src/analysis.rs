//! Analyzers : comment un champ `text` est decoupe en termes.
//!
//! ferrite n'expose que des analyzers **integres**, construits a partir des
//! briques de tantivy. Chacun est compare token par token a son homonyme
//! d'Elasticsearch par `tests/compat/diff_analyzers.py` : ceux qui ne
//! coincident pas sont refuses plutot que livres sous un nom qui promettrait le
//! comportement d'ES.

use tantivy::tokenizer::{
    Language, LowerCaser, RawTokenizer, RemoveLongFilter, StopWordFilter, TextAnalyzer,
    TokenizerManager, WhitespaceTokenizer,
};

use crate::error::{EsError, EsResult};

/// La longueur maximale d'un token, comme `standard` chez ES.
const MAX_TOKEN_LEN: usize = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Analyzer {
    /// Le defaut d'ES : decoupe sur les non-alphanumeriques, minuscules.
    #[default]
    Standard,
    /// Comme `standard`, mais les chiffres sont des separateurs.
    Simple,
    /// Decoupe sur les espaces uniquement, **sans** passer en minuscules.
    Whitespace,
    /// Aucune decoupe : la valeur entiere devient un seul token.
    Keyword,
    /// `standard` + mots vides anglais.
    Stop,
}

impl Analyzer {
    pub fn parse(nom: &str) -> Option<Self> {
        Some(match nom {
            "standard" | "default" => Self::Standard,
            "simple" => Self::Simple,
            "whitespace" => Self::Whitespace,
            "keyword" => Self::Keyword,
            "stop" => Self::Stop,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Simple => "simple",
            Self::Whitespace => "whitespace",
            Self::Keyword => "keyword",
            Self::Stop => "stop",
        }
    }

    /// Le nom sous lequel l'analyzer est enregistre aupres de tantivy.
    ///
    /// Prefixe pour ne pas entrer en collision avec les tokenizers que tantivy
    /// enregistre lui-meme (`default`, `raw`, ...).
    pub fn tokenizer(self) -> &'static str {
        match self {
            Self::Standard => "fr_standard",
            Self::Simple => "fr_simple",
            Self::Whitespace => "fr_whitespace",
            Self::Keyword => "fr_keyword",
            Self::Stop => "fr_stop",
        }
    }

    fn build(self) -> TextAnalyzer {
        match self {
            Self::Standard => TextAnalyzer::builder(StandardTokenizer)
                .filter(RemoveLongFilter::limit(MAX_TOKEN_LEN))
                .filter(LowerCaser)
                .build(),
            Self::Simple => TextAnalyzer::builder(LetterTokenizer)
                .filter(LowerCaser)
                .build(),
            Self::Whitespace => TextAnalyzer::builder(WhitespaceTokenizer::default())
                .filter(RemoveLongFilter::limit(MAX_TOKEN_LEN))
                .build(),
            Self::Keyword => TextAnalyzer::builder(RawTokenizer::default()).build(),
            // ES batit `stop` sur le tokenizer « lettres » (les chiffres sont
            // donc des separateurs), pas sur `standard`.
            Self::Stop => TextAnalyzer::builder(LetterTokenizer)
                .filter(LowerCaser)
                .filter(StopWordFilter::new(Language::English).unwrap())
                .build(),
        }
    }
}

/// Enregistre tous les analyzers integres aupres d'un index.
pub fn register_all(manager: &TokenizerManager) {
    for a in [
        Analyzer::Standard,
        Analyzer::Simple,
        Analyzer::Whitespace,
        Analyzer::Keyword,
        Analyzer::Stop,
    ] {
        manager.register(a.tokenizer(), a.build());
    }
}

/// Les analyzers d'ES que ferrite refuse **volontairement**, avec la raison.
fn refus_explicite(nom: &str) -> Option<&'static str> {
    match nom {
        "french" | "english" | "german" | "spanish" | "italian" | "portuguese" | "dutch"
        | "russian" | "swedish" | "norwegian" | "danish" | "finnish" | "hungarian" | "romanian"
        | "turkish" | "snowball" => Some(
            "les analyzers de langue reposent sur un stemmer, et celui de tantivy (Snowball) \
             n'est pas celui de Lucene (stemmer leger pour le francais, Porter pour l'anglais) : \
             les termes produits different. Mesure sur 28 textes : 17 donnent des termes \
             differents en [french], 19 en [english] — par exemple « Horla » devient [horl] chez \
             tantivy et [horla] chez ES, « mineurs » [mineur] contre [mineu]. Porter le nom d'ES \
             en indexant autre chose changerait silencieusement les resultats d'un mapping \
             existant",
        ),
        _ => None,
    }
}

/// Lit le nom d'un analyzer, en refusant explicitement ceux qu'on ne sait pas
/// reproduire fidelement.
pub fn parse_declaration(nom: &str, champ: &str) -> EsResult<Analyzer> {
    if let Some(raison) = refus_explicite(nom) {
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas l'analyzer [{nom}] (champ [{champ}]) : {raison} (voir \
             docs/compat.md)"
        )));
    }
    Analyzer::parse(nom).ok_or_else(|| {
        EsError::unsupported(format!(
            "ferrite ne supporte pas l'analyzer [{nom}] (champ [{champ}]) ; analyzers \
             integres : standard, simple, whitespace, keyword, stop. Les analyzers definis dans \
             [settings.analysis] ne sont pas supportes."
        ))
    })
}

/// Le tokenizer de l'analyzer `standard` d'ES : les frontieres de mots
/// d'Unicode (UAX#29), celles qu'applique Lucene.
///
/// C'est ce qui garde `l'ascension` en un seul terme, la ou une decoupe sur les
/// non-alphanumeriques en ferait deux (`l` et `ascension`) — et donnerait donc
/// des resultats de recherche differents de ceux d'ES sur tout texte francais.
#[derive(Clone, Default)]
struct StandardTokenizer;

impl tantivy::tokenizer::Tokenizer for StandardTokenizer {
    type TokenStream<'a> = VecTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        use unicode_segmentation::UnicodeSegmentation;
        let mut tokens = Vec::new();
        let mut position = 0usize;
        for (offset, mot) in text.split_word_bound_indices() {
            // Lucene ne garde que les segments qui portent de l'alphanumerique :
            // la ponctuation et les espaces sont des separateurs.
            if !mot.chars().any(char::is_alphanumeric) {
                continue;
            }
            tokens.push(tantivy::tokenizer::Token {
                offset_from: offset,
                offset_to: offset + mot.len(),
                position,
                text: mot.to_string(),
                position_length: 1,
            });
            position += 1;
        }
        VecTokenStream { tokens, index: 0 }
    }
}

pub struct VecTokenStream {
    tokens: Vec<tantivy::tokenizer::Token>,
    index: usize,
}

impl tantivy::tokenizer::TokenStream for VecTokenStream {
    fn advance(&mut self) -> bool {
        self.index += 1;
        self.index <= self.tokens.len()
    }

    fn token(&self) -> &tantivy::tokenizer::Token {
        &self.tokens[self.index - 1]
    }

    fn token_mut(&mut self) -> &mut tantivy::tokenizer::Token {
        &mut self.tokens[self.index - 1]
    }
}

// ---------------------------------------------------------------------------
// Briques absentes de tantivy
// ---------------------------------------------------------------------------

/// Le tokenizer de l'analyzer `simple` d'ES : coupe a chaque caractere qui
/// n'est pas une **lettre** — les chiffres sont donc des separateurs, la ou le
/// `SimpleTokenizer` de tantivy les garde.
#[derive(Clone, Default)]
struct LetterTokenizer;

impl tantivy::tokenizer::Tokenizer for LetterTokenizer {
    type TokenStream<'a> = LetterTokenStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        LetterTokenStream {
            text,
            position: usize::MAX,
            offset: 0,
            token: tantivy::tokenizer::Token::default(),
        }
    }
}

struct LetterTokenStream<'a> {
    text: &'a str,
    position: usize,
    offset: usize,
    token: tantivy::tokenizer::Token,
}

impl tantivy::tokenizer::TokenStream for LetterTokenStream<'_> {
    fn advance(&mut self) -> bool {
        let reste = &self.text[self.offset..];
        let debut_relatif = match reste.char_indices().find(|(_, c)| c.is_alphabetic()) {
            Some((i, _)) => i,
            None => {
                self.offset = self.text.len();
                return false;
            }
        };
        let debut = self.offset + debut_relatif;
        let fin = match self.text[debut..]
            .char_indices()
            .find(|(_, c)| !c.is_alphabetic())
        {
            Some((i, _)) => debut + i,
            None => self.text.len(),
        };
        self.position = self.position.wrapping_add(1);
        self.token.offset_from = debut;
        self.token.offset_to = fin;
        self.token.position = self.position;
        self.token.text.clear();
        self.token.text.push_str(&self.text[debut..fin]);
        self.offset = fin;
        true
    }

    fn token(&self) -> &tantivy::tokenizer::Token {
        &self.token
    }

    fn token_mut(&mut self) -> &mut tantivy::tokenizer::Token {
        &mut self.token
    }
}

/// Decoupe un texte avec un analyzer donne, pour l'API `_analyze`.
pub fn analyser(
    manager: &TokenizerManager,
    analyzer: Analyzer,
    texte: &str,
) -> EsResult<Vec<Token>> {
    let mut ta = manager.get(analyzer.tokenizer()).ok_or_else(|| {
        EsError::internal(format!("analyzer [{}] non enregistre", analyzer.name()))
    })?;
    let mut flux = ta.token_stream(texte);
    let mut out = Vec::new();
    while flux.advance() {
        let t = flux.token();
        out.push(Token {
            text: t.text.clone(),
            start_offset: t.offset_from,
            end_offset: t.offset_to,
            position: t.position,
        });
    }
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct Token {
    pub text: String,
    pub start_offset: usize,
    pub end_offset: usize,
    pub position: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(a: Analyzer, texte: &str) -> Vec<String> {
        let manager = TokenizerManager::default();
        register_all(&manager);
        analyser(&manager, a, texte)
            .unwrap()
            .into_iter()
            .map(|t| t.text)
            .collect()
    }

    #[test]
    fn standard_minuscule_et_decoupe() {
        assert_eq!(
            tokens(Analyzer::Standard, "Bel-Ami 1885"),
            ["bel", "ami", "1885"]
        );
    }

    /// La difference qui changeait les resultats sur tout texte francais :
    /// `standard` doit garder `l'ascension` en un seul terme, comme ES.
    #[test]
    fn standard_garde_les_elisions() {
        assert_eq!(
            tokens(Analyzer::Standard, "l'ascension sociale d'un arriviste"),
            ["l'ascension", "sociale", "d'un", "arriviste"]
        );
    }

    #[test]
    fn simple_coupe_sur_les_chiffres() {
        // C'est la difference avec `standard` : les chiffres sont separateurs.
        assert_eq!(tokens(Analyzer::Simple, "Bel-Ami 1885"), ["bel", "ami"]);
    }

    #[test]
    fn whitespace_ne_minuscule_pas() {
        assert_eq!(
            tokens(Analyzer::Whitespace, "Bel-Ami Zola"),
            ["Bel-Ami", "Zola"]
        );
    }

    #[test]
    fn keyword_garde_tout() {
        assert_eq!(tokens(Analyzer::Keyword, "Bel-Ami Zola"), ["Bel-Ami Zola"]);
    }
}
