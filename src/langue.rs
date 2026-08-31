//! Les analyzers de langue d'Elasticsearch.
//!
//! # Ce que la mesure a dit, et pourquoi le refus est tombe
//!
//! Ces analyzers etaient refuses en bloc, sur une raison qui paraissait
//! solide : « le stemmer de tantivy (Snowball) n'est pas celui de Lucene ».
//! Elle est **fausse pour la plupart des langues**, et c'est une mesure qui l'a
//! dit — pas une lecture. Sur les vocabulaires du projet Snowball (BSD, 20 000
//! a 96 000 mots par langue, que personne ici n'a ecrits), le stem de tantivy
//! est **identique a l'octet** a celui d'Elasticsearch sur le neerlandais, le
//! suedois, le norvegien, le danois, le hongrois, le roumain, le turc et
//! l'anglais Porter2. Ce qui manquait, ce n'etait pas l'algorithme :
//!
//! - **la liste de mots vides** de chaque langue ([`crate::mots_vides`]) ;
//! - **quatre filtres** qu'ES pose autour du stemmer et que tantivy n'a pas :
//!   la normalisation allemande, l'elision italienne, l'apostrophe turque et
//!   les minuscules turques ;
//! - **quatre stemmers legers** (Savoy), parce que l'allemand, l'espagnol,
//!   l'italien et le portugais d'ES **n'emploient pas Snowball** — la meme
//!   famille que le `french_light` deja porte ([`crate::stemmer`]).
//!
//! Le finnois reste refuse, avec son chiffre : 13 ecarts sur 84 399 mots, tous
//! des emprunts a diacritiques etrangers (`garcía`, `bundesstraße`), ou
//! `rust-stemmers` coupe la voyelle finale que Snowball garde. Un analyzer
//! n'est jamais livre sous le nom d'ES tant qu'il n'est pas mesure identique :
//! un ecart de 0,015 % rendu en 200 est exactement ce que ce depot refuse.
//!
//! La mesure se refait par `tests/compat/sonde_langues.py`.

use tantivy::tokenizer::{Language, TextAnalyzer, Token, TokenFilter, TokenStream, Tokenizer};

use crate::analysis::{DecoupeLong, Reecrit, StandardTokenizer};
use crate::mots_vides;

/// Une langue dont ferrite reproduit l'analyzer d'ES a l'identique.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Langue {
    Allemand,
    Danois,
    Espagnol,
    Hongrois,
    Italien,
    Neerlandais,
    Norvegien,
    Portugais,
    Roumain,
    Russe,
    Suedois,
    Turc,
}

impl Langue {
    pub fn parse(nom: &str) -> Option<Self> {
        Some(match nom {
            "german" => Self::Allemand,
            "danish" => Self::Danois,
            "spanish" => Self::Espagnol,
            "hungarian" => Self::Hongrois,
            "italian" => Self::Italien,
            "dutch" => Self::Neerlandais,
            "norwegian" => Self::Norvegien,
            "portuguese" => Self::Portugais,
            "romanian" => Self::Roumain,
            "russian" => Self::Russe,
            "swedish" => Self::Suedois,
            "turkish" => Self::Turc,
            _ => return None,
        })
    }

    pub fn nom(self) -> &'static str {
        match self {
            Self::Allemand => "german",
            Self::Danois => "danish",
            Self::Espagnol => "spanish",
            Self::Hongrois => "hungarian",
            Self::Italien => "italian",
            Self::Neerlandais => "dutch",
            Self::Norvegien => "norwegian",
            Self::Portugais => "portuguese",
            Self::Roumain => "romanian",
            Self::Russe => "russian",
            Self::Suedois => "swedish",
            Self::Turc => "turkish",
        }
    }

    /// Toutes les langues servies, dans l'ordre alphabetique de leur nom d'ES.
    pub const TOUTES: [Self; 12] = [
        Self::Danois,
        Self::Neerlandais,
        Self::Allemand,
        Self::Hongrois,
        Self::Italien,
        Self::Norvegien,
        Self::Portugais,
        Self::Roumain,
        Self::Russe,
        Self::Espagnol,
        Self::Suedois,
        Self::Turc,
    ];

    /// La liste `_{langue}_` d'Elasticsearch, un mot par ligne.
    pub fn mots_vides(self) -> &'static str {
        match self {
            Self::Allemand => mots_vides::GERMAN,
            Self::Danois => mots_vides::DANISH,
            Self::Espagnol => mots_vides::SPANISH,
            Self::Hongrois => mots_vides::HUNGARIAN,
            Self::Italien => mots_vides::ITALIAN,
            Self::Neerlandais => mots_vides::DUTCH,
            Self::Norvegien => mots_vides::NORWEGIAN,
            Self::Portugais => mots_vides::PORTUGUESE,
            Self::Roumain => mots_vides::ROMANIAN,
            Self::Russe => mots_vides::RUSSIAN,
            Self::Suedois => mots_vides::SWEDISH,
            Self::Turc => mots_vides::TURKISH,
        }
    }

    /// Le stemmer que l'analyzer de cette langue pose en dernier.
    ///
    /// Ce n'est pas toujours celui que `{"type": "stemmer", "language":
    /// "<langue>"}` designe : pour quatre langues, l'analyzer nomme pose un
    /// stemmer **leger** alors que le filtre du meme nom pose Snowball. Mesure :
    /// sur les 35 053 mots du vocabulaire allemand, la chaine batie avec
    /// Snowball s'ecarte de l'analyzer `german` sur 445 mots, celle batie avec
    /// le stemmer leger sur 0.
    pub fn stemmer(self) -> Stemmer {
        match self {
            Self::Allemand => Stemmer::LegerAllemand,
            Self::Espagnol => Stemmer::LegerEspagnol,
            Self::Italien => Stemmer::LegerItalien,
            Self::Portugais => Stemmer::LegerPortugais,
            Self::Danois => Stemmer::Snowball(Language::Danish),
            Self::Hongrois => Stemmer::Snowball(Language::Hungarian),
            Self::Neerlandais => Stemmer::Snowball(Language::Dutch),
            Self::Norvegien => Stemmer::Snowball(Language::Norwegian),
            Self::Roumain => Stemmer::Snowball(Language::Romanian),
            Self::Russe => Stemmer::Snowball(Language::Russian),
            Self::Suedois => Stemmer::Snowball(Language::Swedish),
            Self::Turc => Stemmer::Snowball(Language::Turkish),
        }
    }
}

/// Le stemmer d'un filtre `stemmer`, ou de la fin d'un analyzer de langue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stemmer {
    /// Snowball, tel que tantivy l'expose — mesure identique a celui de Lucene.
    Snowball(Language),
    /// Le Porter **original**, que Lucene appelle `PorterStemFilter` et qu'ES
    /// nomme `english` : ce n'est pas le Snowball anglais (`porter2`), et la
    /// difference se voit tout de suite — `quickly` rend `quickli` chez l'un et
    /// `quick` chez l'autre.
    Porter,
    LegerFrancais,
    LegerAllemand,
    LegerEspagnol,
    LegerItalien,
    LegerPortugais,
}

impl Stemmer {
    /// Le nom sous lequel ES accepte ce stemmer dans `{"type": "stemmer"}`.
    pub fn nom(self) -> &'static str {
        match self {
            Self::Snowball(Language::Danish) => "danish",
            Self::Snowball(Language::Dutch) => "dutch",
            Self::Snowball(Language::English) => "porter2",
            Self::Snowball(Language::German) => "german",
            Self::Snowball(Language::Hungarian) => "hungarian",
            Self::Snowball(Language::Italian) => "italian",
            Self::Snowball(Language::Norwegian) => "norwegian",
            Self::Snowball(Language::Portuguese) => "portuguese",
            Self::Snowball(Language::Romanian) => "romanian",
            Self::Snowball(Language::Russian) => "russian",
            Self::Snowball(Language::Spanish) => "spanish",
            Self::Snowball(Language::Swedish) => "swedish",
            Self::Snowball(Language::Turkish) => "turkish",
            // Les autres langues de `rust-stemmers` ne sont pas servies : rien
            // ne peut donc en construire une.
            Self::Snowball(_) => unreachable!("stemmer non servi"),
            Self::Porter => "english",
            Self::LegerFrancais => "light_french",
            Self::LegerAllemand => "light_german",
            Self::LegerEspagnol => "light_spanish",
            Self::LegerItalien => "light_italian",
            Self::LegerPortugais => "light_portuguese",
        }
    }

    /// Lit un `language` de filtre `stemmer`. `None` : ES le connait
    /// peut-etre, ferrite ne sait pas le reproduire — l'appelant refuse alors
    /// en le nommant.
    pub fn parse(nom: &str) -> Option<Self> {
        Some(match nom {
            // Les noms sont exactement ceux qu'ES accepte, releves en les lui
            // posant un par un : il n'a **aucun** alias a deux lettres (`nl`,
            // `de`, `fr` sont refuses chez lui), et `porter_stem` est un type
            // de filtre, pas une langue.
            "danish" => Self::Snowball(Language::Danish),
            "dutch" => Self::Snowball(Language::Dutch),
            "hungarian" => Self::Snowball(Language::Hungarian),
            "norwegian" => Self::Snowball(Language::Norwegian),
            "romanian" => Self::Snowball(Language::Romanian),
            "russian" => Self::Snowball(Language::Russian),
            "swedish" => Self::Snowball(Language::Swedish),
            "turkish" => Self::Snowball(Language::Turkish),
            "porter2" => Self::Snowball(Language::English),
            // Snowball pour ces quatre langues aussi : c'est un stemmer
            // **different** de celui que l'analyzer du meme nom pose (qui est
            // leger). Les deux sont servis, sous leurs deux noms.
            "german" => Self::Snowball(Language::German),
            "spanish" => Self::Snowball(Language::Spanish),
            "italian" => Self::Snowball(Language::Italian),
            "portuguese" => Self::Snowball(Language::Portuguese),
            "english" => Self::Porter,
            "light_french" => Self::LegerFrancais,
            "light_german" => Self::LegerAllemand,
            "light_spanish" => Self::LegerEspagnol,
            "light_italian" => Self::LegerItalien,
            "light_portuguese" => Self::LegerPortugais,
            _ => return None,
        })
    }

    /// Les noms acceptes, pour le message d'un refus.
    pub const NOMS: [&'static str; 19] = [
        "danish",
        "dutch",
        "hungarian",
        "norwegian",
        "romanian",
        "russian",
        "swedish",
        "turkish",
        "porter2",
        "german",
        "spanish",
        "italian",
        "portuguese",
        "english",
        "light_french",
        "light_german",
        "light_spanish",
        "light_italian",
        "light_portuguese",
    ];

    /// L'algorithme Snowball correspondant, s'il y en a un.
    fn algorithme(self) -> Option<rust_stemmers::Algorithm> {
        use rust_stemmers::Algorithm as A;
        Some(match self {
            Self::Snowball(Language::Danish) => A::Danish,
            Self::Snowball(Language::Dutch) => A::Dutch,
            Self::Snowball(Language::English) => A::English,
            Self::Snowball(Language::German) => A::German,
            Self::Snowball(Language::Hungarian) => A::Hungarian,
            Self::Snowball(Language::Italian) => A::Italian,
            Self::Snowball(Language::Norwegian) => A::Norwegian,
            Self::Snowball(Language::Portuguese) => A::Portuguese,
            Self::Snowball(Language::Romanian) => A::Romanian,
            Self::Snowball(Language::Russian) => A::Russian,
            Self::Snowball(Language::Spanish) => A::Spanish,
            Self::Snowball(Language::Swedish) => A::Swedish,
            Self::Snowball(Language::Turkish) => A::Turkish,
            Self::Snowball(_) => return None,
            _ => return None,
        })
    }

    /// Le mot rendu tel quel, sans passer par Snowball ? C'est le
    /// `StemmerOverrideFilter` que `DutchAnalyzer` pose avant son stemmer : un
    /// dictionnaire de **quatre** mots, qu'il marque comme mots-cles pour que
    /// le stemmer ne les touche pas.
    ///
    /// Quatre mots sur 45 670, et c'est exactement pourquoi le corpus doit etre
    /// large : sur 3 000 mots tires au sort, aucun des quatre ne sortait.
    fn exception(self, mot: &str) -> Option<&'static str> {
        if self != Self::Snowball(Language::Dutch) {
            return None;
        }
        Some(match mot {
            "fiets" => "fiets",
            "bromfiets" => "bromfiets",
            "ei" => "eier",
            "kind" => "kinder",
            _ => return None,
        })
    }

    pub fn pose(
        self,
        b: tantivy::tokenizer::TextAnalyzerBuilder,
    ) -> tantivy::tokenizer::TextAnalyzerBuilder {
        match self {
            Self::Porter => b.filter_dynamic(Reecrit(|t| Some(crate::stemmer::porter(t)))),
            Self::LegerFrancais => {
                b.filter_dynamic(Reecrit(|t| Some(crate::stemmer::french_light(t))))
            }
            Self::LegerAllemand => {
                b.filter_dynamic(Reecrit(|t| Some(crate::stemmer::german_light(t))))
            }
            Self::LegerEspagnol => {
                b.filter_dynamic(Reecrit(|t| Some(crate::stemmer::spanish_light(t))))
            }
            Self::LegerItalien => {
                b.filter_dynamic(Reecrit(|t| Some(crate::stemmer::italian_light(t))))
            }
            Self::LegerPortugais => {
                b.filter_dynamic(Reecrit(|t| Some(crate::stemmer::portuguese_light(t))))
            }
            Self::Snowball(_) => b.filter_dynamic(FiltreSnowball(self)),
        }
    }

    /// Le stem d'un mot, prelude et exceptions compris.
    fn stem(self, stemmer: &rust_stemmers::Stemmer, mot: &str) -> Option<String> {
        if let Some(fige) = self.exception(mot) {
            return Some(fige.to_string());
        }
        // Le `prelude` de l'algorithme russe de Snowball : « ё » devient « е »
        // partout dans le mot. `rust-stemmers` ne l'applique pas — sans lui,
        // 112 mots du vocabulaire russe sur 49 785 sortent autrement que chez
        // ES (« костёр » reste entier au lieu de rendre « костер »).
        let entree = if self == Self::Snowball(Language::Russian) && mot.contains('ё') {
            std::borrow::Cow::Owned(mot.replace('ё', "е"))
        } else {
            std::borrow::Cow::Borrowed(mot)
        };
        let stemme = stemmer.stem(&entree);
        // La comparaison porte sur le token **d'origine**, pas sur l'entree du
        // stemmer : un mot que le prelude a change et que Snowball laisse
        // ensuite tel quel doit quand meme etre reecrit.
        (stemme.as_ref() != mot).then(|| stemme.into_owned())
    }
}

/// Le filtre qui applique un algorithme Snowball, ses regles de bord comprises.
#[derive(Clone)]
pub struct FiltreSnowball(Stemmer);

impl TokenFilter for FiltreSnowball {
    type Tokenizer<T: Tokenizer> = SnowballTokenizer<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> SnowballTokenizer<T> {
        SnowballTokenizer {
            tokenizer,
            config: self.0,
        }
    }
}

#[derive(Clone)]
pub struct SnowballTokenizer<T> {
    tokenizer: T,
    config: Stemmer,
}

impl<T: Tokenizer> Tokenizer for SnowballTokenizer<T> {
    type TokenStream<'a> = SnowballStream<T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        SnowballStream {
            config: self.config,
            // Construit une fois par flux, comme le fait tantivy.
            stemmer: rust_stemmers::Stemmer::create(
                self.config.algorithme().expect("un algorithme Snowball"),
            ),
            tail: self.tokenizer.token_stream(text),
        }
    }
}

pub struct SnowballStream<T> {
    config: Stemmer,
    stemmer: rust_stemmers::Stemmer,
    tail: T,
}

impl<T: TokenStream> TokenStream for SnowballStream<T> {
    fn advance(&mut self) -> bool {
        if !self.tail.advance() {
            return false;
        }
        if let Some(nouveau) = self.config.stem(&self.stemmer, &self.tail.token().text) {
            self.tail.token_mut().text = nouveau;
        }
        true
    }

    fn token(&self) -> &Token {
        self.tail.token()
    }

    fn token_mut(&mut self) -> &mut Token {
        self.tail.token_mut()
    }
}

/// Les articles elides que `ItalianAnalyzer` retire en tete de token.
pub const ELISIONS_IT: &[&str] = &[
    "c", "l", "all", "dall", "dell", "nell", "sull", "coll", "pell", "gl", "agl", "dagl", "degl",
    "negl", "sugl", "un", "m", "t", "s", "v", "d",
];

/// Assemble la chaine d'analyse d'une langue, dans l'ordre exact d'ES.
///
/// L'ordre n'est pas decoratif : le filtre d'apostrophe turc passe **avant**
/// les minuscules (sinon `Diyarbakır'ın` garde son apostrophe et son suffixe),
/// l'elision italienne aussi (elle est insensible a la casse), et la
/// normalisation allemande **apres** les mots vides — elle ne connait que les
/// lettres basses, et un mot vide normalise n'est plus le mot vide de la liste.
pub fn construit(langue: Langue) -> TextAnalyzer {
    let b = TextAnalyzer::builder(StandardTokenizer)
        .dynamic()
        .filter_dynamic(DecoupeLong::limit(crate::analysis::MAX_TOKEN_LEN));
    let b = match langue {
        Langue::Turc => b
            .filter_dynamic(Reecrit(apostrophe))
            .filter_dynamic(Reecrit(minuscule_turque)),
        Langue::Italien => b
            .filter_dynamic(Elision::statique(ELISIONS_IT))
            .filter_dynamic(Reecrit(minuscule)),
        _ => b.filter_dynamic(Reecrit(minuscule)),
    };
    let b = b.filter_dynamic(crate::analysis::filtre_mots_vides(langue.mots_vides()));
    let b = match langue {
        Langue::Allemand => b.filter_dynamic(Reecrit(normalisation_allemande)),
        _ => b,
    };
    langue.stemmer().pose(b).build()
}

/// L'analyzer `snowball` d'ES : `standard`, minuscules, mots vides anglais,
/// puis le Snowball **anglais** — donc `porter2`, et non le Porter original de
/// l'analyzer `english`. Mesure : `quickly` rend `quick` ici et `quickli` la.
pub fn construit_snowball() -> TextAnalyzer {
    let b = TextAnalyzer::builder(StandardTokenizer)
        .dynamic()
        .filter_dynamic(DecoupeLong::limit(crate::analysis::MAX_TOKEN_LEN))
        .filter_dynamic(Reecrit(minuscule))
        .filter_dynamic(crate::analysis::filtre_mots_vides(mots_vides::ENGLISH));
    Stemmer::Snowball(Language::English).pose(b).build()
}

// ---------------------------------------------------------------------------
// Les filtres que tantivy n'a pas
// ---------------------------------------------------------------------------

/// Les minuscules de Java (`Character.toLowerCase`), qui ne sont pas celles de
/// Rust.
///
/// Deux ecarts, mesures caractere par caractere contre ES sur 1 433
/// caracteres :
///
/// - `to_lowercase` de Rust rend **deux** caracteres pour `İ` (U+0130) : un `i`
///   suivi du point suscrit combinant. Java n'a pas de repli long, il rend
///   `i` ; et `İ` est le seul caractere d'Unicode dont le repli inconditionnel
///   fasse plus d'un caractere, donc garder le premier suffit ;
/// - le `LowerCaser` de tantivy ne replie que ce que `is_uppercase` accepte,
///   ce qui laisse les 31 caracteres **titre** (`ǅ`, `ᾈ`…) tels quels.
///
/// Ces 32 caracteres sortaient donc de ferrite tels quels la ou ES les replie —
/// sur `standard` comme sur tout analyzer sur mesure, en silence.
pub fn minuscule(t: &str) -> Option<String> {
    if !t.chars().any(|c| c.to_lowercase().next() != Some(c)) {
        return None;
    }
    Some(
        t.chars()
            .map(|c| c.to_lowercase().next().unwrap_or(c))
            .collect(),
    )
}

/// `ApostropheFilter` de Lucene : tout ce qui suit une apostrophe disparait,
/// l'apostrophe comprise.
///
/// En turc, l'apostrophe separe le suffixe d'un nom propre (`Diyarbakır'ın`) ;
/// Lucene pose le filtre avant le stemmer. Sans lui, 213 mots du vocabulaire
/// turc sur 3 000 tires au sort sortent autrement que chez ES.
pub fn apostrophe(t: &str) -> Option<String> {
    t.find(['\'', '\u{2019}']).map(|i| t[..i].to_string())
}

/// `TurkishLowerCaseFilter` de Lucene : `I` sans point devient `ı`, et le point
/// suscrit combinant qui suit un `i` disparait.
///
/// Sans lui, `ISTANBUL` rend `istanbul` la ou le turc veut `ıstanbul` — donc un
/// document qu'une recherche en turc ne retrouve pas.
fn minuscule_turque(t: &str) -> Option<String> {
    let cs: Vec<char> = t.chars().collect();
    let mut out = String::with_capacity(t.len());
    let mut apres_i = false;
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        apres_i = c == 'I' || (apres_i && crate::ngram::est_marque_non_espacante(c));
        if apres_i {
            match c {
                // Le point suscrit qui suivait un `I` : Java le supprime pour
                // imiter la forme composee.
                '\u{0307}' => {
                    i += 1;
                    continue;
                }
                'I' => {
                    if suivi_du_point(&cs[i + 1..]) {
                        out.push('i');
                    } else {
                        out.push('ı');
                        apres_i = false;
                    }
                    i += 1;
                    continue;
                }
                _ => {}
            }
        }
        out.push(c.to_lowercase().next().unwrap_or(c));
        i += 1;
    }
    Some(out)
}

/// Un point suscrit combinant suit-il, eventuellement derriere d'autres
/// marques non espacantes ?
fn suivi_du_point(reste: &[char]) -> bool {
    for c in reste {
        if !crate::ngram::est_marque_non_espacante(*c) {
            return false;
        }
        if *c == '\u{0307}' {
            return true;
        }
    }
    false
}

/// `GermanNormalizationFilter` de Lucene, son automate a trois etats compris.
///
/// C'est ce qui la rend impossible a deviner : le `e` d'un digramme `ae` / `oe`
/// / `ue` ne tombe que si la voyelle qui le precede a ouvert l'etat trema.
/// `quelle` garde donc son `e` (le `q` ferme l'etat), `haeuser` le perd.
pub fn normalisation_allemande(t: &str) -> Option<String> {
    const N: u8 = 0; // etat ordinaire
    const V: u8 = 1; // empeche un `u` d'ouvrir l'etat trema
    const U: u8 = 2; // etat trema : un `e` y tombe
    let mut etat = N;
    let mut out = String::with_capacity(t.len());
    for c in t.chars() {
        match c {
            'a' | 'o' => {
                etat = U;
                out.push(c);
            }
            'u' => {
                etat = if etat == N { U } else { V };
                out.push(c);
            }
            'e' => {
                if etat != U {
                    out.push('e');
                }
                etat = V;
            }
            'i' | 'q' | 'y' => {
                etat = V;
                out.push(c);
            }
            'ä' => {
                etat = V;
                out.push('a');
            }
            'ö' => {
                etat = V;
                out.push('o');
            }
            'ü' => {
                etat = V;
                out.push('u');
            }
            'ß' => {
                etat = N;
                out.push_str("ss");
            }
            autre => {
                etat = N;
                out.push(autre);
            }
        }
    }
    Some(out)
}

/// Le filtre `elision` : un article elide en tete de token disparait.
///
/// Deux formes : la liste figee d'un analyzer de langue, et celle qu'un
/// `analysis.filter` declare.
#[derive(Debug, Clone, PartialEq)]
pub struct Elision {
    articles: Articles,
    /// `articles_case` chez ES, et il veut l'inverse de ce que son nom
    /// suggere : il est passe a un `CharArraySet` en guise de `ignoreCase`,
    /// donc `true` compare **sans** tenir compte de la casse. Le defaut
    /// (`false`) compare exactement — et c'est mesure : sur `["l", "d"]`, ES
    /// laisse `L'anno` entier par defaut et rend `anno` avec
    /// `articles_case: true`.
    ///
    /// Les analyzers `french` et `italian` posent tous deux `true` : leur
    /// elision agit avant les minuscules, donc sans ca elle ne servirait a rien
    /// sur un debut de phrase.
    ignore_casse: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Articles {
    Figes(&'static [&'static str]),
    Declares(std::sync::Arc<Vec<String>>),
}

impl Elision {
    /// La liste figee d'un analyzer de langue : toujours insensible a la casse.
    pub fn statique(articles: &'static [&'static str]) -> Self {
        Self {
            articles: Articles::Figes(articles),
            ignore_casse: true,
        }
    }

    pub fn declares(articles: Vec<String>, ignore_casse: bool) -> Self {
        Self {
            articles: Articles::Declares(std::sync::Arc::new(articles)),
            ignore_casse,
        }
    }

    /// La forme persistee, relisible par le parseur de `analysis.filter`.
    pub fn to_json(&self) -> serde_json::Value {
        let articles: Vec<String> = match &self.articles {
            Articles::Figes(a) => a.iter().map(|s| (*s).to_string()).collect(),
            Articles::Declares(a) => a.as_ref().clone(),
        };
        serde_json::json!({
            "type": "elision",
            "articles": articles,
            "articles_case": self.ignore_casse,
        })
    }

    fn elide(&self, t: &str) -> Option<String> {
        let (avant, apres) = t.split_once(['\'', '\u{2019}'])?;
        let candidat = if self.ignore_casse {
            std::borrow::Cow::Owned(avant.to_lowercase())
        } else {
            std::borrow::Cow::Borrowed(avant)
        };
        let present = match &self.articles {
            Articles::Figes(a) => a.contains(&candidat.as_ref()),
            Articles::Declares(a) => a.iter().any(|x| x == candidat.as_ref()),
        };
        present.then(|| apres.to_string())
    }
}

impl TokenFilter for Elision {
    type Tokenizer<T: Tokenizer> = ElisionTokenizer<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> ElisionTokenizer<T> {
        ElisionTokenizer {
            tokenizer,
            config: self,
        }
    }
}

#[derive(Clone)]
pub struct ElisionTokenizer<T> {
    tokenizer: T,
    config: Elision,
}

impl<T: Tokenizer> Tokenizer for ElisionTokenizer<T> {
    type TokenStream<'a> = ElisionStream<T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        ElisionStream {
            config: self.config.clone(),
            tail: self.tokenizer.token_stream(text),
        }
    }
}

pub struct ElisionStream<T> {
    config: Elision,
    tail: T,
}

impl<T: TokenStream> TokenStream for ElisionStream<T> {
    fn advance(&mut self) -> bool {
        if !self.tail.advance() {
            return false;
        }
        if let Some(nouveau) = self.config.elide(&self.tail.token().text) {
            self.tail.token_mut().text = nouveau;
        }
        true
    }

    fn token(&self) -> &Token {
        self.tail.token()
    }

    fn token_mut(&mut self) -> &mut Token {
        self.tail.token_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Les cas que la documentation de Lucene donne pour
    /// `german_normalization`, plus les deux ou le `e` **ne tombe pas**.
    #[test]
    fn normalisation_allemande_sur_les_cas_de_reference() {
        for (mot, attendu) in [
            ("häuser", "hauser"),
            ("haeuser", "hauser"),
            ("straße", "strasse"),
            ("strasse", "strasse"),
            // `qu` ferme l'etat trema : le `e` reste.
            ("quelle", "quelle"),
            // Un `ue` en tete de mot : le `u` ouvre l'etat, donc le `e` tombe.
            ("ueber", "uber"),
        ] {
            assert_eq!(
                normalisation_allemande(mot).as_deref(),
                Some(attendu),
                "sur {mot}"
            );
        }
    }

    #[test]
    fn minuscules_turques() {
        // `I` sans point suscrit derriere : `ı`, la voyelle sans point.
        assert_eq!(minuscule_turque("ISTANBUL").as_deref(), Some("ıstanbul"));
        // `İ` : `i`, et le point disparait — sous ses deux ecritures.
        assert_eq!(minuscule_turque("İSTANBUL").as_deref(), Some("istanbul"));
        assert_eq!(
            minuscule_turque("I\u{0307}STANBUL").as_deref(),
            Some("istanbul")
        );
    }

    /// Les 32 caracteres qu'un repli fonde sur `is_uppercase` laisse passer.
    #[test]
    fn minuscules_de_java_et_pas_de_rust() {
        assert_eq!(minuscule("İ").as_deref(), Some("i"));
        assert_eq!(minuscule("ǅ").as_deref(), Some("ǆ"));
        assert_eq!(minuscule("ᾈ").as_deref(), Some("ᾀ"));
        assert_eq!(minuscule("deja lu"), None);
    }

    #[test]
    fn apostrophe_turque() {
        assert_eq!(apostrophe("diyarbakır'ın").as_deref(), Some("diyarbakır"));
        assert_eq!(apostrophe("ev"), None);
    }

    /// L'elision d'un analyzer de langue ignore la casse — elle passe avant
    /// les minuscules, donc c'est la seule facon qu'elle serve a quelque chose.
    /// Un `elision` **declare**, lui, est sensible a la casse par defaut :
    /// `articles_case` est un `ignoreCase`, pas un `caseSensitive`.
    #[test]
    fn elision_italienne_et_articles_case() {
        let e = Elision::statique(ELISIONS_IT);
        assert_eq!(e.elide("dell'anno").as_deref(), Some("anno"));
        assert_eq!(e.elide("Dell'Anno").as_deref(), Some("Anno"));
        assert_eq!(e.elide("zzz'anno"), None);
        assert_eq!(e.elide("anno"), None);

        let defaut = Elision::declares(vec!["l".into(), "d".into()], false);
        assert_eq!(defaut.elide("l'anno").as_deref(), Some("anno"));
        assert_eq!(defaut.elide("L'anno"), None, "le defaut compare exactement");
        let ignore = Elision::declares(vec!["l".into(), "d".into()], true);
        assert_eq!(ignore.elide("L'anno").as_deref(), Some("anno"));
    }

    /// Le prelude russe et le dictionnaire neerlandais : deux regles qui
    /// vivent **dans** le stemmer, et qu'aucun echantillon n'avait montrees.
    #[test]
    fn les_bords_du_stemmer_snowball() {
        let russe = Stemmer::Snowball(Language::Russian);
        let s = rust_stemmers::Stemmer::create(russe.algorithme().unwrap());
        assert_eq!(russe.stem(&s, "костёр").as_deref(), Some("костер"));

        let nl = Stemmer::Snowball(Language::Dutch);
        let s = rust_stemmers::Stemmer::create(nl.algorithme().unwrap());
        assert_eq!(nl.stem(&s, "fiets").as_deref(), Some("fiets"));
        assert_eq!(nl.stem(&s, "ei").as_deref(), Some("eier"));
        assert_eq!(nl.stem(&s, "kind").as_deref(), Some("kinder"));
        // Et le meme mot sous un autre stemmer n'a pas d'exception.
        let da = Stemmer::Snowball(Language::Danish);
        let s = rust_stemmers::Stemmer::create(da.algorithme().unwrap());
        assert_eq!(da.stem(&s, "ei"), None);
    }
}
