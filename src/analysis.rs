//! Analyzers : comment un champ `text` est decoupe en termes.
//!
//! ferrite n'expose que des analyzers **integres**, construits a partir des
//! briques de tantivy. Chacun est compare token par token a son homonyme
//! d'Elasticsearch par `tests/compat/diff_analyzers.py` : ceux qui ne
//! coincident pas sont refuses plutot que livres sous un nom qui promettrait le
//! comportement d'ES.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};
use tantivy::tokenizer::{
    AsciiFoldingFilter, Language, LowerCaser, RawTokenizer, RemoveLongFilter, StopWordFilter,
    TextAnalyzer, TokenizerManager, WhitespaceTokenizer,
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
    /// Un analyzer declare dans `settings.analysis`, par sa position dans la
    /// liste triee de l'index.
    Custom(u16),
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

    /// Le nom de l'analyzer tel qu'il a ete declare.
    pub fn name(self, analysis: &Analysis) -> String {
        match self {
            Self::Standard => "standard".into(),
            Self::Simple => "simple".into(),
            Self::Whitespace => "whitespace".into(),
            Self::Keyword => "keyword".into(),
            Self::Stop => "stop".into(),
            Self::Custom(i) => analysis
                .sur_mesure
                .get(i as usize)
                .map(|a| a.nom.clone())
                .unwrap_or_else(|| nom_interne(i)),
        }
    }

    /// Le nom sous lequel l'analyzer est enregistre aupres de tantivy.
    ///
    /// Prefixe pour ne pas entrer en collision avec les tokenizers que tantivy
    /// enregistre lui-meme (`default`, `raw`, ...).
    pub fn tokenizer(self) -> String {
        match self {
            Self::Standard => "fr_standard".into(),
            Self::Simple => "fr_simple".into(),
            Self::Whitespace => "fr_whitespace".into(),
            Self::Keyword => "fr_keyword".into(),
            Self::Stop => "fr_stop".into(),
            Self::Custom(i) => nom_interne(i),
        }
    }

    fn build(self) -> TextAnalyzer {
        match self {
            // Les analyzers sur mesure sont enregistres par `Analysis::register`.
            Self::Custom(_) => TextAnalyzer::builder(RawTokenizer::default()).build(),
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
        manager.register(&a.tokenizer(), a.build());
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
pub fn parse_declaration(nom: &str, champ: &str, analysis: &Analysis) -> EsResult<Analyzer> {
    if let Some(i) = analysis.index_de(nom) {
        return Ok(Analyzer::Custom(i));
    }
    if let Some(raison) = refus_explicite(nom) {
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas l'analyzer [{nom}] (champ [{champ}]) : {raison} (voir \
             docs/compat.md)"
        )));
    }
    Analyzer::parse(nom).ok_or_else(|| {
        EsError::unsupported(format!(
            "ferrite ne supporte pas l'analyzer [{nom}] (champ [{champ}]) ; analyzers \
             integres : standard, simple, whitespace, keyword, stop, et ceux declares dans \
             [settings.analysis]"
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
    let nom = analyzer.tokenizer();
    let mut ta = manager
        .get(&nom)
        .ok_or_else(|| EsError::internal(format!("analyzer [{nom}] non enregistre")))?;
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

// ---------------------------------------------------------------------------
// Analyzers sur mesure (`settings.analysis`)
// ---------------------------------------------------------------------------

/// La section `analysis` des `settings` d'un index.
///
/// Un mapping venu d'une instance reelle declare presque toujours un analyzer
/// sur mesure — le plus souvent `standard` + `lowercase` + `asciifolding`, pour
/// que « Éditions » et « editions » se retrouvent. Ces briques-la, ferrite les
/// a ; ce sont les **stemmers** qui manquent, et eux seuls restent refuses.
///
/// L'ordre de la liste est celui des noms tries : il fixe l'identifiant sous
/// lequel chaque analyzer est enregistre (`fr_c0`, `fr_c1`...), donc il doit
/// rester stable d'une generation a l'autre.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Analysis {
    pub sur_mesure: Vec<CustomAnalyzer>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CustomAnalyzer {
    pub nom: String,
    tokenizer: Tok,
    filtres: Vec<Filtre>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    Standard,
    Whitespace,
    Keyword,
    Letter,
    Lowercase,
}

#[derive(Debug, Clone, PartialEq)]
enum Filtre {
    Lowercase,
    AsciiFolding,
    Stop(Vec<String>),
}

impl Analysis {
    pub fn est_vide(&self) -> bool {
        self.sur_mesure.is_empty()
    }

    pub fn index_de(&self, nom: &str) -> Option<u16> {
        self.sur_mesure
            .iter()
            .position(|a| a.nom == nom)
            .map(|i| i as u16)
    }

    /// Parse `settings.analysis`. Tout ce qui n'est pas compris est refuse :
    /// un filtre ignore changerait les termes indexes sans le dire.
    pub fn parse(v: &Value) -> EsResult<Self> {
        let obj = v
            .as_object()
            .ok_or_else(|| EsError::mapper_parsing("[analysis] doit etre un objet"))?;
        for cle in obj.keys() {
            if !matches!(cle.as_str(), "analyzer" | "filter" | "tokenizer") {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas [analysis.{cle}] ; sections acceptees : analyzer, \
                     filter"
                )));
            }
        }
        if let Some(t) = obj.get("tokenizer") {
            if t.as_object().is_some_and(|o| !o.is_empty()) {
                return Err(EsError::unsupported(
                    "ferrite ne supporte pas les tokenizers definis dans [analysis.tokenizer] ; \
                     tokenizers integres : standard, whitespace, keyword, letter, lowercase",
                ));
            }
        }

        // Les filtres nommes, resolus d'abord : un analyzer peut les citer.
        let mut nommes: BTreeMap<String, Filtre> = BTreeMap::new();
        if let Some(f) = obj.get("filter") {
            let f = f
                .as_object()
                .ok_or_else(|| EsError::mapper_parsing("[analysis.filter] doit etre un objet"))?;
            for (nom, decl) in f {
                nommes.insert(nom.clone(), Filtre::parse_declare(nom, decl)?);
            }
        }

        let mut sur_mesure = Vec::new();
        if let Some(a) = obj.get("analyzer") {
            let a = a
                .as_object()
                .ok_or_else(|| EsError::mapper_parsing("[analysis.analyzer] doit etre un objet"))?;
            for (nom, decl) in a {
                sur_mesure.push(CustomAnalyzer::parse(nom, decl, &nommes)?);
            }
        }
        sur_mesure.sort_by(|a, b| a.nom.cmp(&b.nom));
        Ok(Self { sur_mesure })
    }

    /// La forme persistee — et re-lisible par [`Self::parse`].
    ///
    /// La symetrie n'est pas un detail : un filtre rendu en ligne la ou le
    /// parseur attend un **nom** casse le redemarrage du serveur, et ca ne se
    /// voit qu'au redemarrage. Les filtres qui portent une configuration sont
    /// donc extraits dans `filter`, et cites par leur nom.
    pub fn to_json(&self) -> Value {
        let mut analyzers = Map::new();
        let mut filtres = Map::new();
        for a in &self.sur_mesure {
            let mut noms = Vec::new();
            for f in &a.filtres {
                match f.nom_integre() {
                    Some(n) => noms.push(json!(n)),
                    None => {
                        let n = format!("_f{}", filtres.len());
                        filtres.insert(n.clone(), f.to_json());
                        noms.push(json!(n));
                    }
                }
            }
            analyzers.insert(
                a.nom.clone(),
                json!({
                    "type": "custom",
                    "tokenizer": a.tokenizer.name(),
                    "filter": noms,
                }),
            );
        }
        json!({"analyzer": analyzers, "filter": filtres})
    }

    /// Enregistre les analyzers sur mesure aupres d'un index.
    pub fn register(&self, manager: &TokenizerManager) {
        for (i, a) in self.sur_mesure.iter().enumerate() {
            manager.register(&nom_interne(i as u16), a.build());
        }
    }
}

/// Le nom sous lequel un analyzer sur mesure est connu de tantivy.
pub fn nom_interne(i: u16) -> String {
    format!("fr_c{i}")
}

impl CustomAnalyzer {
    fn parse(nom: &str, decl: &Value, nommes: &BTreeMap<String, Filtre>) -> EsResult<Self> {
        let obj = decl.as_object().ok_or_else(|| {
            EsError::mapper_parsing(format!("[analysis.analyzer.{nom}] doit etre un objet"))
        })?;
        for cle in obj.keys() {
            if !matches!(
                cle.as_str(),
                "type" | "tokenizer" | "filter" | "char_filter"
            ) {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas [{cle}] dans l'analyzer [{nom}] ; parametres \
                     acceptes : type, tokenizer, filter"
                )));
            }
        }
        if obj.contains_key("char_filter") {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas les [char_filter] (analyzer [{nom}])"
            )));
        }
        match obj.get("type").and_then(Value::as_str) {
            None | Some("custom") => {}
            Some(autre) => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas un analyzer de type [{autre}] dans \
                     [analysis.analyzer.{nom}] ; seul [custom] est accepte (les analyzers de \
                     langue restent refuses, voir docs/compat.md)"
                )))
            }
        }
        let tokenizer = match obj.get("tokenizer").and_then(Value::as_str) {
            Some(t) => Tok::parse(t, nom)?,
            None => {
                return Err(EsError::mapper_parsing(format!(
                    "[analysis.analyzer.{nom}] : un analyzer [custom] declare son [tokenizer]"
                )))
            }
        };
        let mut filtres = Vec::new();
        if let Some(liste) = obj.get("filter") {
            let liste = liste.as_array().ok_or_else(|| {
                EsError::mapper_parsing(format!("[analysis.analyzer.{nom}.filter] est une liste"))
            })?;
            for f in liste {
                let cle = f.as_str().ok_or_else(|| {
                    EsError::mapper_parsing(format!(
                        "[analysis.analyzer.{nom}.filter] : noms de filtres attendus"
                    ))
                })?;
                filtres.push(match nommes.get(cle) {
                    Some(f) => f.clone(),
                    None => Filtre::integre(cle, nom)?,
                });
            }
        }
        Ok(Self {
            nom: nom.to_string(),
            tokenizer,
            filtres,
        })
    }

    fn build(&self) -> TextAnalyzer {
        let mut b = match self.tokenizer {
            Tok::Standard => TextAnalyzer::builder(StandardTokenizer).dynamic(),
            Tok::Whitespace => TextAnalyzer::builder(WhitespaceTokenizer::default()).dynamic(),
            Tok::Keyword => TextAnalyzer::builder(RawTokenizer::default()).dynamic(),
            Tok::Letter | Tok::Lowercase => TextAnalyzer::builder(LetterTokenizer).dynamic(),
        };
        // Le tokenizer `lowercase` d'ES, c'est `letter` + minuscules.
        if self.tokenizer == Tok::Lowercase {
            b = b.filter_dynamic(LowerCaser);
        }
        b = b.filter_dynamic(RemoveLongFilter::limit(MAX_TOKEN_LEN));
        for f in &self.filtres {
            b = match f {
                Filtre::Lowercase => b.filter_dynamic(LowerCaser),
                Filtre::AsciiFolding => b.filter_dynamic(AsciiFoldingFilter),
                Filtre::Stop(mots) => {
                    b.filter_dynamic(StopWordFilter::remove(mots.iter().cloned()))
                }
            };
        }
        b.build()
    }
}

impl Tok {
    fn parse(nom: &str, analyzer: &str) -> EsResult<Self> {
        Ok(match nom {
            "standard" => Self::Standard,
            "whitespace" => Self::Whitespace,
            "keyword" => Self::Keyword,
            "letter" => Self::Letter,
            "lowercase" => Self::Lowercase,
            autre => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas le tokenizer [{autre}] (analyzer [{analyzer}]) ; \
                     tokenizers integres : standard, whitespace, keyword, letter, lowercase"
                )))
            }
        })
    }

    fn name(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Whitespace => "whitespace",
            Self::Keyword => "keyword",
            Self::Letter => "letter",
            Self::Lowercase => "lowercase",
        }
    }
}

impl Filtre {
    /// Un filtre cite par son nom integre (`lowercase`, `asciifolding`...).
    fn integre(nom: &str, analyzer: &str) -> EsResult<Self> {
        Ok(match nom {
            "lowercase" => Self::Lowercase,
            "asciifolding" => Self::AsciiFolding,
            "stop" => Self::Stop(MOTS_VIDES_EN.iter().map(|s| (*s).to_string()).collect()),
            autre => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas le filtre [{autre}] (analyzer [{analyzer}]) ; \
                     filtres integres : lowercase, asciifolding, stop. Les filtres a base de \
                     stemmer restent refuses (voir docs/compat.md)"
                )))
            }
        })
    }

    /// Un filtre declare dans `analysis.filter`.
    fn parse_declare(nom: &str, decl: &Value) -> EsResult<Self> {
        let obj = decl.as_object().ok_or_else(|| {
            EsError::mapper_parsing(format!("[analysis.filter.{nom}] doit etre un objet"))
        })?;
        let ty = obj.get("type").and_then(Value::as_str).ok_or_else(|| {
            EsError::mapper_parsing(format!("[analysis.filter.{nom}] : [type] manquant"))
        })?;
        Ok(match ty {
            "stop" => {
                let mots = match obj.get("stopwords") {
                    None => MOTS_VIDES_EN.iter().map(|s| (*s).to_string()).collect(),
                    Some(Value::Array(a)) => a
                        .iter()
                        .map(|v| {
                            v.as_str().map(str::to_string).ok_or_else(|| {
                                EsError::mapper_parsing(format!(
                                    "[analysis.filter.{nom}.stopwords] : chaines attendues"
                                ))
                            })
                        })
                        .collect::<EsResult<Vec<_>>>()?,
                    Some(Value::String(s)) if s == "_english_" => {
                        MOTS_VIDES_EN.iter().map(|s| (*s).to_string()).collect()
                    }
                    Some(Value::String(s)) => {
                        return Err(EsError::unsupported(format!(
                            "ferrite ne supporte pas la liste de mots vides [{s}] \
                             (filtre [{nom}]) ; accepte : _english_, ou une liste explicite"
                        )))
                    }
                    Some(_) => {
                        return Err(EsError::mapper_parsing(format!(
                            "[analysis.filter.{nom}.stopwords] : liste ou nom attendu"
                        )))
                    }
                };
                Self::Stop(mots)
            }
            "lowercase" => Self::Lowercase,
            "asciifolding" => Self::AsciiFolding,
            autre => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas un filtre de type [{autre}] (filtre [{nom}]) ; \
                     types acceptes : stop, lowercase, asciifolding"
                )))
            }
        })
    }

    /// Le nom integre du filtre, s'il n'a pas de configuration propre.
    fn nom_integre(&self) -> Option<&'static str> {
        match self {
            Self::Lowercase => Some("lowercase"),
            Self::AsciiFolding => Some("asciifolding"),
            Self::Stop(_) => None,
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Self::Lowercase => json!({"type": "lowercase"}),
            Self::AsciiFolding => json!({"type": "asciifolding"}),
            Self::Stop(mots) => json!({"type": "stop", "stopwords": mots}),
        }
    }
}

/// Les mots vides anglais de Lucene (`ENGLISH_STOP_WORDS_SET`).
const MOTS_VIDES_EN: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "if", "in", "into", "is", "it",
    "no", "not", "of", "on", "or", "such", "that", "the", "their", "then", "there", "these",
    "they", "this", "to", "was", "will", "with",
];

#[cfg(test)]
mod tests_analysis {
    use super::*;

    /// Ce que la persistance ecrit doit se relire — sinon le serveur ne
    /// redemarre pas, et ca ne se voit qu'au redemarrage.
    #[test]
    fn la_serialisation_fait_l_aller_retour() {
        let decl = json!({
            "analyzer": {
                "fr_produit": {"type": "custom", "tokenizer": "standard",
                               "filter": ["lowercase", "asciifolding"]},
                "sans_vides": {"type": "custom", "tokenizer": "standard",
                               "filter": ["lowercase", "mes_vides"]},
                "brut": {"type": "custom", "tokenizer": "keyword"}
            },
            "filter": {"mes_vides": {"type": "stop", "stopwords": ["le", "la"]}}
        });
        let a = Analysis::parse(&decl).unwrap();
        assert_eq!(a.sur_mesure.len(), 3);
        let relu = Analysis::parse(&a.to_json()).unwrap();
        assert_eq!(a, relu, "rendu : {}", a.to_json());
        // Et les noms declares restent les memes, donc les champs les retrouvent.
        assert_eq!(a.index_de("fr_produit"), relu.index_de("fr_produit"));
    }

    #[test]
    fn ce_qui_n_est_pas_reproductible_est_refuse() {
        for decl in [
            json!({"analyzer": {"x": {"type": "custom", "tokenizer": "standard",
                                      "filter": ["porter_stem"]}}}),
            json!({"analyzer": {"x": {"type": "custom", "tokenizer": "ngram"}}}),
            json!({"analyzer": {"x": {"type": "french"}}}),
            json!({"analyzer": {"x": {"type": "custom", "tokenizer": "standard",
                                      "char_filter": ["html_strip"]}}}),
        ] {
            assert!(
                Analysis::parse(&decl).is_err(),
                "aurait du refuser : {decl}"
            );
        }
    }
}
