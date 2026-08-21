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
    /// `standard` + elision + minuscules + mots vides + stemmer leger.
    French,
    /// `standard` + possessif + minuscules + mots vides + Porter.
    English,
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
            "french" => Self::French,
            "english" => Self::English,
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
            Self::French => "french".into(),
            Self::English => "english".into(),
            Self::Custom(i) => analysis
                .sur_mesure
                .get(i as usize)
                .map(|a| a.nom.clone())
                .unwrap_or_else(|| nom_interne(i)),
        }
    }

    /// Un analyzer declare dans `settings.analysis` ?
    ///
    /// Ce n'est pas cosmetique : `_analyze` sur plusieurs textes intercale un
    /// **saut de position** entre eux, et ES le prend de l'analyzer — 100 pour
    /// un analyzer sur mesure, 0 pour un integre (mesure : `standard` sur
    /// `["abc de", "fg"]` place `fg` en position 2, le meme decoupage sur mesure
    /// en position 102).
    pub fn est_sur_mesure(self) -> bool {
        matches!(self, Self::Custom(_))
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
            Self::French => "fr_french".into(),
            Self::English => "fr_english".into(),
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
            // L'ordre est celui de `FrenchAnalyzer` chez Lucene : l'elision
            // agit **avant** les minuscules (elle est insensible a la casse),
            // et le stemmer en dernier.
            Self::French => TextAnalyzer::builder(StandardTokenizer)
                .filter(RemoveLongFilter::limit(MAX_TOKEN_LEN))
                .filter(Reecrit(elision))
                .filter(LowerCaser)
                .filter(StopWordFilter::remove(
                    MOTS_VIDES_FR.iter().map(|s| (*s).to_string()),
                ))
                .filter(Reecrit(french_light))
                .build(),
            // `EnglishAnalyzer` : le possessif avant les minuscules, Porter en
            // dernier.
            Self::English => TextAnalyzer::builder(StandardTokenizer)
                .filter(RemoveLongFilter::limit(MAX_TOKEN_LEN))
                .filter(Reecrit(possessif))
                .filter(LowerCaser)
                .filter(StopWordFilter::remove(
                    MOTS_VIDES_EN.iter().map(|s| (*s).to_string()),
                ))
                .filter(Reecrit(porter))
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
        Analyzer::French,
        Analyzer::English,
    ] {
        manager.register(&a.tokenizer(), a.build());
    }
}

/// Les analyzers d'ES que ferrite refuse **volontairement**, avec la raison.
fn refus_explicite(nom: &str) -> Option<&'static str> {
    match nom {
        "german" | "spanish" | "italian" | "portuguese" | "dutch" | "russian" | "swedish"
        | "norwegian" | "danish" | "finnish" | "hungarian" | "romanian" | "turkish"
        | "snowball" => Some(
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
             integres : standard, simple, whitespace, keyword, stop, french, english, et ceux \
             declares dans [settings.analysis]"
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

impl VecTokenStream {
    pub fn new(tokens: Vec<tantivy::tokenizer::Token>) -> Self {
        Self { tokens, index: 0 }
    }
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
    Ok(analyser_avec(&mut ta, texte))
}

/// Le meme, sur un `TextAnalyzer` deja construit — c'est ce dont se sert
/// `_analyze` quand le corps declare son `tokenizer` et ses `filter` en ligne,
/// sans passer par un index.
pub fn analyser_avec(ta: &mut TextAnalyzer, texte: &str) -> Vec<Token> {
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
    out
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Standard,
    Whitespace,
    Keyword,
    Letter,
    Lowercase,
    /// `ngram` ou `edge_ngram`, declare dans `analysis.tokenizer`.
    Ngram(crate::ngram::NgramTokenizer),
}

#[derive(Debug, Clone, PartialEq)]
enum Filtre {
    Lowercase,
    AsciiFolding,
    Stop(Vec<String>),
    /// `ngram` ou `edge_ngram`, cote filtre.
    Ngram(crate::ngram::NgramFilter),
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
    ///
    /// `max_ngram_diff` vient de `settings.index.max_ngram_diff` : c'est le
    /// seul reglage d'index qui **valide** une declaration d'analyse, donc il
    /// doit etre lu avant elle.
    pub fn parse(v: &Value, max_ngram_diff: i64) -> EsResult<Self> {
        let obj = v
            .as_object()
            .ok_or_else(|| EsError::mapper_parsing("[analysis] doit etre un objet"))?;
        for cle in obj.keys() {
            if !matches!(cle.as_str(), "analyzer" | "filter" | "tokenizer") {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas [analysis.{cle}] ; sections acceptees : analyzer, \
                     filter, tokenizer"
                )));
            }
        }

        // Les tokenizers nommes, puis les filtres nommes : un analyzer les cite
        // par leur nom, donc ils se resolvent avant lui.
        let mut tokenizers: BTreeMap<String, Tok> = BTreeMap::new();
        if let Some(t) = obj.get("tokenizer") {
            let t = t.as_object().ok_or_else(|| {
                EsError::mapper_parsing("[analysis.tokenizer] doit etre un objet")
            })?;
            for (nom, decl) in t {
                tokenizers.insert(nom.clone(), Tok::parse_declare(nom, decl, max_ngram_diff)?);
            }
        }

        let mut nommes: BTreeMap<String, Filtre> = BTreeMap::new();
        if let Some(f) = obj.get("filter") {
            let f = f
                .as_object()
                .ok_or_else(|| EsError::mapper_parsing("[analysis.filter] doit etre un objet"))?;
            for (nom, decl) in f {
                nommes.insert(
                    nom.clone(),
                    Filtre::parse_declare(nom, decl, max_ngram_diff)?,
                );
            }
        }

        let mut sur_mesure = Vec::new();
        if let Some(a) = obj.get("analyzer") {
            let a = a
                .as_object()
                .ok_or_else(|| EsError::mapper_parsing("[analysis.analyzer] doit etre un objet"))?;
            for (nom, decl) in a {
                sur_mesure.push(CustomAnalyzer::parse(nom, decl, &tokenizers, &nommes)?);
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
        let mut tokenizers = Map::new();
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
            // Meme raison que pour les filtres configures : un tokenizer rendu
            // en ligne la ou le parseur attend un **nom** casse le redemarrage.
            let tokenizer = match a.tokenizer.name() {
                Some(n) => json!(n),
                None => {
                    let n = format!("_t{}", tokenizers.len());
                    tokenizers.insert(n.clone(), a.tokenizer.to_json());
                    json!(n)
                }
            };
            analyzers.insert(
                a.nom.clone(),
                json!({
                    "type": "custom",
                    "tokenizer": tokenizer,
                    "filter": noms,
                }),
            );
        }
        json!({"analyzer": analyzers, "filter": filtres, "tokenizer": tokenizers})
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
    fn parse(
        nom: &str,
        decl: &Value,
        tokenizers: &BTreeMap<String, Tok>,
        nommes: &BTreeMap<String, Filtre>,
    ) -> EsResult<Self> {
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
            Some(t) => match tokenizers.get(t) {
                Some(tok) => tok.clone(),
                None => Tok::parse(t, nom)?,
            },
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

    /// L'analyzer **anonyme** de `_analyze` : un `tokenizer` et des `filter`
    /// donnes en ligne, sans index ni declaration.
    ///
    /// C'est ce qui rend une brique d'analyse mesurable toute seule — poser la
    /// meme question aux deux serveurs ne demande alors ni index ni mapping.
    /// Chaque element accepte les deux ecritures d'ES : un **nom** (`"ngram"`,
    /// qui prend ses defauts) ou un **objet** (`{"type": "ngram", …}`).
    pub fn en_ligne(
        tokenizer: Option<&Value>,
        filtres: Option<&Value>,
        max_ngram_diff: i64,
    ) -> EsResult<Self> {
        let tokenizer = match tokenizer {
            None => Tok::Standard,
            Some(Value::String(s)) => {
                Tok::parse_declare("_analyze", &json!({ "type": s.clone() }), max_ngram_diff)?
            }
            Some(v @ Value::Object(_)) => Tok::parse_declare("_analyze", v, max_ngram_diff)?,
            Some(_) => {
                return Err(EsError::illegal_argument(
                    "[_analyze.tokenizer] : un nom ou un objet est attendu",
                ))
            }
        };
        let mut liste = Vec::new();
        if let Some(f) = filtres {
            let f = f.as_array().ok_or_else(|| {
                EsError::illegal_argument("[_analyze.filter] : une liste est attendue")
            })?;
            for x in f {
                liste.push(match x {
                    Value::String(s) => Filtre::integre(s, "_analyze")?,
                    v @ Value::Object(_) => Filtre::parse_declare("_analyze", v, max_ngram_diff)?,
                    _ => {
                        return Err(EsError::illegal_argument(
                            "[_analyze.filter] : des noms ou des objets sont attendus",
                        ))
                    }
                });
            }
        }
        Ok(Self {
            nom: "_analyze".into(),
            tokenizer,
            filtres: liste,
        })
    }

    /// Le `TextAnalyzer` correspondant, pour l'exercer hors d'un index.
    pub fn analyseur(&self) -> TextAnalyzer {
        self.build()
    }

    fn build(&self) -> TextAnalyzer {
        let mut b = match &self.tokenizer {
            Tok::Standard => TextAnalyzer::builder(StandardTokenizer).dynamic(),
            Tok::Whitespace => TextAnalyzer::builder(WhitespaceTokenizer::default()).dynamic(),
            Tok::Keyword => TextAnalyzer::builder(RawTokenizer::default()).dynamic(),
            Tok::Letter | Tok::Lowercase => TextAnalyzer::builder(LetterTokenizer).dynamic(),
            Tok::Ngram(t) => TextAnalyzer::builder(t.clone()).dynamic(),
        };
        // Le tokenizer `lowercase` d'ES, c'est `letter` + minuscules.
        if self.tokenizer == Tok::Lowercase {
            b = b.filter_dynamic(LowerCaser);
        }
        // La limite de 255 est celle des tokenizers de Lucene batis sur
        // `CharTokenizer` — `standard`, `whitespace`, `letter`, `lowercase`.
        // `keyword` et les n-grammes n'en ont pas, et l'appliquer quand meme
        // videerait un `keyword` + filtre `edge_ngram` pose sur un titre long,
        // en silence, la ou ES rend ses grammes.
        if self.tokenizer.limite_de_token() {
            b = b.filter_dynamic(RemoveLongFilter::limit(MAX_TOKEN_LEN));
        }
        for f in &self.filtres {
            b = match f {
                Filtre::Lowercase => b.filter_dynamic(LowerCaser),
                Filtre::AsciiFolding => b.filter_dynamic(AsciiFoldingFilter),
                Filtre::Stop(mots) => {
                    b.filter_dynamic(StopWordFilter::remove(mots.iter().cloned()))
                }
                Filtre::Ngram(n) => b.filter_dynamic(n.clone()),
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
            // Cites sans bornes, ils prennent les defauts d'ES (1 et 2) — dont
            // l'ecart vaut 1, donc toujours dans la limite par defaut.
            "ngram" | "edge_ngram" => {
                Self::Ngram(crate::ngram::NgramTokenizer::defaut(nom == "edge_ngram"))
            }
            autre => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas le tokenizer [{autre}] (analyzer [{analyzer}]) ; \
                     tokenizers integres : standard, whitespace, keyword, letter, lowercase, \
                     ngram, edge_ngram, et ceux declares dans [analysis.tokenizer]"
                )))
            }
        })
    }

    /// Un tokenizer declare dans `analysis.tokenizer`.
    fn parse_declare(nom: &str, decl: &Value, max_ngram_diff: i64) -> EsResult<Self> {
        let obj = decl.as_object().ok_or_else(|| {
            EsError::mapper_parsing(format!("[analysis.tokenizer.{nom}] doit etre un objet"))
        })?;
        let ty = obj.get("type").and_then(Value::as_str).ok_or_else(|| {
            EsError::mapper_parsing(format!("[analysis.tokenizer.{nom}] : [type] manquant"))
        })?;
        let ou = format!("analysis.tokenizer.{nom}");
        Ok(match ty {
            "ngram" | "edge_ngram" => Self::Ngram(crate::ngram::NgramTokenizer::parse(
                obj,
                ty == "edge_ngram",
                &ou,
                max_ngram_diff,
            )?),
            "standard" => Self::Standard,
            "whitespace" => Self::Whitespace,
            "keyword" => Self::Keyword,
            "letter" => Self::Letter,
            "lowercase" => Self::Lowercase,
            autre => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas un tokenizer de type [{autre}] (tokenizer [{nom}]) ; \
                     types acceptes : ngram, edge_ngram, standard, whitespace, keyword, letter, \
                     lowercase"
                )))
            }
        })
    }

    /// Le nom integre du tokenizer, s'il n'a pas de configuration propre.
    fn name(&self) -> Option<&'static str> {
        Some(match self {
            Self::Standard => "standard",
            Self::Whitespace => "whitespace",
            Self::Keyword => "keyword",
            Self::Letter => "letter",
            Self::Lowercase => "lowercase",
            Self::Ngram(_) => return None,
        })
    }

    fn to_json(&self) -> Value {
        match self {
            Self::Ngram(t) => t.to_json(),
            autre => json!({"type": autre.name().unwrap_or("standard")}),
        }
    }

    /// Ce tokenizer coupe-t-il a 255 caracteres, comme le `CharTokenizer` de
    /// Lucene dont il derive ?
    fn limite_de_token(&self) -> bool {
        matches!(
            self,
            Self::Standard | Self::Whitespace | Self::Letter | Self::Lowercase
        )
    }
}

impl Filtre {
    /// Un filtre cite par son nom integre (`lowercase`, `asciifolding`...).
    fn integre(nom: &str, analyzer: &str) -> EsResult<Self> {
        Ok(match nom {
            "lowercase" => Self::Lowercase,
            "asciifolding" => Self::AsciiFolding,
            "stop" => Self::Stop(MOTS_VIDES_EN.iter().map(|s| (*s).to_string()).collect()),
            "ngram" | "edge_ngram" => {
                Self::Ngram(crate::ngram::NgramFilter::defaut(nom == "edge_ngram"))
            }
            autre => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas le filtre [{autre}] (analyzer [{analyzer}]) ; \
                     filtres integres : lowercase, asciifolding, stop, ngram, edge_ngram, et ceux \
                     declares dans [analysis.filter]. Les filtres a base de stemmer restent \
                     refuses (voir docs/compat.md)"
                )))
            }
        })
    }

    /// Un filtre declare dans `analysis.filter`.
    fn parse_declare(nom: &str, decl: &Value, max_ngram_diff: i64) -> EsResult<Self> {
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
            "ngram" | "edge_ngram" => Self::Ngram(crate::ngram::NgramFilter::parse(
                obj,
                ty == "edge_ngram",
                &format!("analysis.filter.{nom}"),
                max_ngram_diff,
            )?),
            autre => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas un filtre de type [{autre}] (filtre [{nom}]) ; \
                     types acceptes : stop, lowercase, asciifolding, ngram, edge_ngram"
                )))
            }
        })
    }

    /// Le nom integre du filtre, s'il n'a pas de configuration propre.
    fn nom_integre(&self) -> Option<&'static str> {
        match self {
            Self::Lowercase => Some("lowercase"),
            Self::AsciiFolding => Some("asciifolding"),
            Self::Stop(_) | Self::Ngram(_) => None,
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Self::Lowercase => json!({"type": "lowercase"}),
            Self::AsciiFolding => json!({"type": "asciifolding"}),
            Self::Stop(mots) => json!({"type": "stop", "stopwords": mots}),
            Self::Ngram(n) => n.to_json(),
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
                "brut": {"type": "custom", "tokenizer": "keyword"},
                // Les deux formes des n-grammes : un tokenizer nomme, et un
                // filtre nomme. Elles se relisent aussi, sinon un index a
                // autocompletion ne redemarrerait pas.
                "auto": {"type": "custom", "tokenizer": "mes_grammes",
                         "filter": ["lowercase"]},
                "auto_filtre": {"type": "custom", "tokenizer": "standard",
                                "filter": ["asciifolding", "edgengram"]}
            },
            "tokenizer": {"mes_grammes": {"type": "edge_ngram", "min_gram": 1, "max_gram": 15,
                                          "token_chars": ["letter", "digit"]}},
            "filter": {"mes_vides": {"type": "stop", "stopwords": ["le", "la"]},
                       "edgengram": {"type": "edge_ngram", "min_gram": 1, "max_gram": 15}}
        });
        let a = Analysis::parse(&decl, 12).unwrap();
        assert_eq!(a.sur_mesure.len(), 5);
        let relu = Analysis::parse(&a.to_json(), 12).unwrap();
        assert_eq!(a, relu, "rendu : {}", a.to_json());
        // Et les noms declares restent les memes, donc les champs les retrouvent.
        assert_eq!(a.index_de("fr_produit"), relu.index_de("fr_produit"));
        assert_eq!(a.index_de("auto"), relu.index_de("auto"));
    }

    #[test]
    fn ce_qui_n_est_pas_reproductible_est_refuse() {
        for decl in [
            json!({"analyzer": {"x": {"type": "custom", "tokenizer": "standard",
                                      "filter": ["porter_stem"]}}}),
            // Le nom d'un tokenizer qui n'existe pas.
            json!({"analyzer": {"x": {"type": "custom", "tokenizer": "pattern"}}}),
            json!({"analyzer": {"x": {"type": "french"}}}),
            json!({"analyzer": {"x": {"type": "custom", "tokenizer": "standard",
                                      "char_filter": ["html_strip"]}}}),
            // L'ecart par defaut est 1 : 3-15 sans `index.max_ngram_diff` est
            // refuse, comme chez ES.
            json!({"tokenizer": {"t": {"type": "ngram", "min_gram": 3, "max_gram": 15}},
                   "analyzer": {"x": {"type": "custom", "tokenizer": "t"}}}),
            json!({"filter": {"f": {"type": "ngram", "min_gram": 3, "max_gram": 15}},
                   "analyzer": {"x": {"type": "custom", "tokenizer": "standard",
                                      "filter": ["f"]}}}),
            json!({"tokenizer": {"t": {"type": "pattern", "pattern": ","}},
                   "analyzer": {"x": {"type": "custom", "tokenizer": "t"}}}),
        ] {
            assert!(
                Analysis::parse(&decl, 1).is_err(),
                "aurait du refuser : {decl}"
            );
        }
    }

    /// Les memes declarations passent des que l'index a releve la borne — et
    /// `edge_ngram` passe sans qu'on l'ait relevee, parce qu'il n'est pas
    /// concerne (mesure contre ES 8.15).
    #[test]
    fn max_ngram_diff_borne_ngram_et_pas_edge_ngram() {
        let ngram = json!({"tokenizer": {"t": {"type": "ngram", "min_gram": 3, "max_gram": 15}},
                           "analyzer": {"x": {"type": "custom", "tokenizer": "t"}}});
        let edge = json!({"tokenizer": {"t": {"type": "edge_ngram", "min_gram": 1, "max_gram": 15}},
                          "analyzer": {"x": {"type": "custom", "tokenizer": "t"}}});
        assert!(Analysis::parse(&ngram, 1).is_err());
        assert!(Analysis::parse(&ngram, 12).is_ok());
        assert!(Analysis::parse(&edge, 1).is_ok());
    }
}

// ---------------------------------------------------------------------------
// Analyzers de langue
// ---------------------------------------------------------------------------

/// Un filtre qui reecrit chaque token par une fonction.
///
/// C'est la brique commune a l'elision, au possessif anglais et aux stemmers :
/// tous se ramenent a « ce token devient celui-la ».
#[derive(Clone)]
pub struct Reecrit(fn(&str) -> Option<String>);

impl tantivy::tokenizer::TokenFilter for Reecrit {
    type Tokenizer<T: tantivy::tokenizer::Tokenizer> = ReecritFilter<T>;

    fn transform<T: tantivy::tokenizer::Tokenizer>(self, tokenizer: T) -> ReecritFilter<T> {
        ReecritFilter {
            tokenizer,
            f: self.0,
        }
    }
}

#[derive(Clone)]
pub struct ReecritFilter<T> {
    tokenizer: T,
    f: fn(&str) -> Option<String>,
}

impl<T: tantivy::tokenizer::Tokenizer> tantivy::tokenizer::Tokenizer for ReecritFilter<T> {
    type TokenStream<'a> = ReecritStream<T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        ReecritStream {
            tail: self.tokenizer.token_stream(text),
            f: self.f,
        }
    }
}

pub struct ReecritStream<T> {
    tail: T,
    f: fn(&str) -> Option<String>,
}

impl<T: tantivy::tokenizer::TokenStream> tantivy::tokenizer::TokenStream for ReecritStream<T> {
    fn advance(&mut self) -> bool {
        if !self.tail.advance() {
            return false;
        }
        if let Some(nouveau) = (self.f)(&self.tail.token().text) {
            self.tail.token_mut().text = nouveau;
        }
        true
    }

    fn token(&self) -> &tantivy::tokenizer::Token {
        self.tail.token()
    }

    fn token_mut(&mut self) -> &mut tantivy::tokenizer::Token {
        self.tail.token_mut()
    }
}

/// Les articles elides que Lucene retire en tete de token (`FrenchAnalyzer`).
const ELISIONS: &[&str] = &[
    "l", "m", "t", "qu", "n", "s", "j", "d", "c", "jusqu", "quoiqu", "lorsqu", "puisqu",
];

/// `l'ascension` -> `ascension`. Applique avant la mise en minuscules, donc
/// insensible a la casse, comme chez Lucene.
fn elision(t: &str) -> Option<String> {
    let (avant, apres) = t.split_once(['\'', '\u{2019}'])?;
    let avant = avant.to_lowercase();
    ELISIONS
        .contains(&avant.as_str())
        .then(|| apres.to_string())
}

/// `Peter's` -> `Peter`. Comme `EnglishPossessiveFilter`, avant les minuscules.
fn possessif(t: &str) -> Option<String> {
    let c: Vec<char> = t.chars().collect();
    let n = c.len();
    if n >= 3 && matches!(c[n - 2], '\'' | '\u{2019}' | '\u{FF07}') && matches!(c[n - 1], 's' | 'S')
    {
        return Some(c[..n - 2].iter().collect());
    }
    None
}

fn porter(t: &str) -> Option<String> {
    Some(crate::stemmer::porter(t))
}

fn french_light(t: &str) -> Option<String> {
    Some(crate::stemmer::french_light(t))
}

/// Les mots vides francais d'Elasticsearch, **relevés** mot a mot plutot que
/// reconstitues : ce n'est ni la liste Snowball (qui garde `est`) ni l'ancienne
/// de Lucene. Le relevé se refait avec `tests/compat/releve_mots_vides.py`, et
/// `diff_analyzers.py` reste l'arbitre.
const MOTS_VIDES_FR: &[&str] = &[
    "ai", "aie", "aient", "aies", "ait", "au", "aurai", "auraient", "aurais", "aurait", "aurez",
    "auriez", "aurions", "aurons", "auront", "aux", "avaient", "avais", "avait", "avec", "avez",
    "aviez", "avons", "ayant", "ayez", "ayons", "c", "ce", "ceci", "cela", "ces", "cet", "cette",
    "d", "dans", "de", "des", "du", "elle", "en", "es", "et", "eu", "eue", "eues", "eurent", "eus",
    "eusse", "eussent", "eusses", "eussiez", "eussions", "eut", "eux", "eûmes", "eût", "eûtes",
    "furent", "fus", "fusse", "fussent", "fusses", "fussiez", "fussions", "fut", "fûmes", "fûtes",
    "ici", "il", "ils", "j", "je", "l", "la", "le", "les", "leur", "leurs", "lui", "m", "ma",
    "mais", "me", "mes", "moi", "mon", "même", "n", "ne", "nos", "notre", "nous", "on", "ont",
    "ou", "par", "pas", "pour", "qu", "que", "quel", "quelle", "quelles", "quels", "qui", "s",
    "sa", "sans", "se", "sera", "serai", "seraient", "serais", "serait", "seras", "serez",
    "seriez", "serions", "serons", "seront", "ses", "soi", "soient", "sois", "soit", "sont",
    "soyez", "soyons", "suis", "sur", "t", "ta", "te", "tes", "toi", "ton", "tu", "un", "une",
    "vos", "votre", "vous", "y", "à", "étaient", "étais", "était", "étant", "étiez", "étions",
    "étée", "étées", "êtes",
];
