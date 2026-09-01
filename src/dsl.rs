//! Traduction du Query DSL d'Elasticsearch vers les requetes tantivy.
//!
//! Contrat du module : **tout ce qui n'est pas traduit fidelement est refuse**.
//! Ignorer un `minimum_should_match` ou une clause inconnue produirait des
//! resultats faux presentes comme complets — le pire resultat possible pour ce
//! projet.

use std::ops::Bound;

use serde_json::{Map, Value};
use tantivy::query::{
    AllQuery, BooleanQuery, BoostQuery, ConstScoreQuery, EmptyQuery, ExistsQuery, FuzzyTermQuery,
    Occur, PhraseQuery, Query, RangeQuery, RegexQuery, TermQuery, TermSetQuery,
};
use tantivy::schema::IndexRecordOption;
use tantivy::{Index, Searcher, Term};

use crate::analysis::Analyzer;
use crate::colonne::{Automate, ColonneQuery, Predicat as PredicatColonne};
use crate::dateformat::DateFormat;
use crate::datemath::{self, Arrondi};
use crate::dismax::DisMaxQuery;
use crate::error::{EsError, EsResult};
use crate::fonction_score::{
    Attenuation, Calcul, Combinaison, Decroissance, Fonction, FonctionScore, ModeDeScore,
    Modificateur, Retrograde, ValeurDeChamp,
};
use crate::mapping::{self, FieldKind, Fields, MappedField, TypedValue};
use crate::nested::{Clause, NestedQuery, Predicat, Valeur};

/// Ce dont la traduction a besoin : le schema resolu et l'index (pour les
/// tokenizers).
pub struct QueryCtx<'a> {
    pub fields: &'a Fields,
    pub index: &'a Index,
    /// La racine `nested` en cours de traduction, s'il y en a une.
    ///
    /// Chez Elasticsearch, les sous-champs d'un `nested` vivent dans des
    /// documents caches : les interroger depuis la racine ne rend **rien**, en
    /// silence. ferrite les indexe sur le parent, il pourrait donc y repondre —
    /// et rendre des documents la ou ES n'en rend aucun. Il refuse a la place,
    /// et ce champ est ce qui distingue « dans une clause `nested` » de
    /// « depuis la racine ».
    pub nested_ouvert: std::cell::RefCell<Vec<String>>,
    /// Un `searcher` sur la meme generation, pour les clauses qui se resolvent
    /// en **deux passes** : `has_child` et `has_parent` executent leur requete
    /// interne a la traduction, materialisent l'ensemble des identifiants
    /// concernes, et rendent une recherche sur ces identifiants. C'est ce que
    /// le mono-shard rend possible sans *global ordinals*.
    pub searcher: &'a Searcher,
    /// Les champs connus d'**un autre** index de la meme recherche.
    ///
    /// Vide sur un index unique : un champ absent est alors une faute de frappe,
    /// et c'est une erreur. En multi-index, un champ que cet index ne mappe pas
    /// mais qu'un autre connait n'est plus une faute : c'est un mapping
    /// heterogene, et la clause ne correspond simplement a rien **ici** — ce que
    /// fait Elasticsearch sur un champ non mappe. Sans ca, ecarter l'index
    /// entier ferait perdre les documents que les *autres* clauses d'un `bool`
    /// auraient trouves.
    pub champs_ailleurs: &'a std::collections::BTreeSet<String>,
    /// Le nom de l'index interroge, pour les clauses qui citent `_index`.
    ///
    /// `None` quand la requete est traduite sans qu'aucun index ne soit vise
    /// (une validation) : `_index` n'y designe alors personne, mais la clause
    /// doit quand meme se construire.
    pub nom_index: Option<&'a str>,
    /// L'instant que `now` designe dans cette requete.
    ///
    /// Pris **une fois** par recherche, comme ES le fait sur son noeud
    /// coordinateur : sans ca, deux bornes de la meme requete (`gte: "now/d"`,
    /// `lt: "now"`) ne parleraient pas du meme instant, et un document indexe
    /// entre les deux pourrait tomber hors de l'intervalle qui le contient.
    pub maintenant: i64,
    /// `index.query.parse.allow_unmapped_fields` de l'index interroge.
    ///
    /// A `true` (le defaut d'ES), une clause sur un champ que le mapping ne
    /// connait pas ne correspond a rien au lieu d'echouer — voir
    /// [`crate::mapping::Mapping::allow_unmapped_fields`].
    pub champs_inconnus_toleres: bool,
    /// La requete est traduite **sans qu'aucun index ne soit vise** : c'est une
    /// validation, pas une recherche (voir [`crate::engine::sans_index`]).
    ///
    /// Les quelques verdicts qui ne peuvent pas se prononcer sans mapping
    /// (`nested` sur un chemin, `has_child` sur un champ `join`) sont alors
    /// suspendus au lieu d'echouer — ES les rend a l'execution d'un shard, et
    /// il n'y a pas de shard. Les suspendre plutot que de laisser l'erreur
    /// sortir n'est pas une politesse : elle masquerait tout ce qui est
    /// **sous** la clause, qui est justement ce qu'on vient valider.
    pub aucun_index_vise: bool,
    /// Ce que l'**execution** rencontrera et qu'ES traite en erreur : un
    /// `field_value_factor` sur un document sans valeur, un score de fonction
    /// negatif. `Scorer::score` ne peut pas echouer ; l'incident est pose ici et
    /// relu apres la recherche (voir [`crate::fonction_score::Incidents`]).
    pub incidents: std::sync::Arc<crate::fonction_score::Incidents>,
}

/// L'ensemble vide, pour les appels qui ne visent qu'un index.
static AUCUN_AUTRE_CHAMP: std::sync::LazyLock<std::collections::BTreeSet<String>> =
    std::sync::LazyLock::new(std::collections::BTreeSet::new);

impl<'a> QueryCtx<'a> {
    pub fn new(fields: &'a Fields, index: &'a Index, searcher: &'a Searcher) -> Self {
        Self {
            fields,
            index,
            nested_ouvert: std::cell::RefCell::new(Vec::new()),
            searcher,
            champs_ailleurs: &AUCUN_AUTRE_CHAMP,
            nom_index: None,
            maintenant: crate::datemath::maintenant(),
            // Le defaut d'Elasticsearch. Le reglage de l'index le resserre, via
            // [`QueryCtx::selon_le_mapping`].
            champs_inconnus_toleres: true,
            aucun_index_vise: false,
            incidents: std::sync::Arc::new(crate::fonction_score::Incidents::anonymes()),
        }
    }

    /// Ou poser les incidents que l'execution rencontrera, et sous quel nom
    /// d'index les rendre.
    pub fn avec_incidents(
        mut self,
        incidents: std::sync::Arc<crate::fonction_score::Incidents>,
    ) -> Self {
        self.incidents = incidents;
        self
    }

    /// Marque le contexte comme « aucun index vise » : la traduction ne sert
    /// qu'a valider le corps (voir [`QueryCtx::aucun_index_vise`]).
    pub fn sans_index_vise(mut self) -> Self {
        self.aucun_index_vise = true;
        self
    }

    /// L'instant que `now` designe : le meme pour tous les index d'une meme
    /// recherche (voir [`QueryCtx::maintenant`]).
    pub fn avec_maintenant(mut self, maintenant: i64) -> Self {
        self.maintenant = maintenant;
        self
    }

    /// Les champs qu'un autre index de la meme recherche connait.
    pub fn avec_champs_ailleurs(mut self, champs: &'a std::collections::BTreeSet<String>) -> Self {
        self.champs_ailleurs = champs;
        self
    }

    /// Le nom de l'index interroge : c'est la valeur que `_index` porte pour
    /// **tous** ses documents.
    pub fn avec_nom_index(mut self, nom: &'a str) -> Self {
        self.nom_index = Some(nom);
        self
    }

    /// Applique les reglages de l'index interroge (`allow_unmapped_fields`).
    pub fn selon_le_mapping(mut self, mapping: &crate::mapping::Mapping) -> Self {
        self.champs_inconnus_toleres = mapping.allow_unmapped_fields;
        self
    }

    /// Cette erreur peut-elle etre rattrapee en « ne correspond a rien » ?
    ///
    /// Deux cas, tous deux mesures sur un vrai ES : le champ est connu d'un
    /// **autre** index de la meme recherche (mapping heterogene), ou l'index
    /// laisse passer les champs non mappes (`allow_unmapped_fields`, le defaut
    /// d'ES).
    fn champ_inconnu_tolere(&self, e: &EsError) -> bool {
        e.champ_inconnu
            .as_deref()
            .is_some_and(|c| self.champs_inconnus_toleres || self.champs_ailleurs.contains(c))
    }
}

impl QueryCtx<'_> {
    fn field(&self, name: &str, clause: &str) -> EsResult<MappedField> {
        if let Some(racine) = self.fields.racine_nested(name) {
            if !self.nested_ouvert.borrow().iter().any(|r| r == racine) {
                return Err(EsError::new(
                    axum::http::StatusCode::BAD_REQUEST,
                    "query_shard_exception",
                    format!(
                        "[{name}] est sous le champ [nested] [{racine}] : il ne peut etre \
                         interroge que dans une clause [nested] sur ce chemin (clause \
                         [{clause}])"
                    ),
                ));
            }
        }
        self.fields.get(name).ok_or_else(|| {
            // Rattrapee plus haut en « ne correspond a rien » quand l'index
            // tolere les champs non mappes (le defaut d'ES) ou qu'un autre index
            // de la meme recherche connait le champ. Si elle sort, c'est que
            // `allow_unmapped_fields` vaut `false` : le message est celui d'ES
            // dans ce cas, et il nomme le reglage a rebasculer.
            EsError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "query_shard_exception",
                format!(
                    "No field mapping can be found for the field with name [{name}] (clause \
                     [{clause}]) ; [index.query.parse.allow_unmapped_fields] vaut [false] sur cet \
                     index"
                ),
            )
            .sur_champ_inconnu(name)
        })
    }

    /// Applique l'analyzer du champ `text` a une chaine de requete.
    ///
    /// Rend les positions en plus des termes : `match_phrase` en a besoin, et
    /// elles ne sont pas toujours consecutives (l'analyzer peut jeter un token).
    fn analyze(&self, text: &str, analyzer: Analyzer) -> EsResult<Vec<(usize, String)>> {
        let mut analyzer = self
            .index
            .tokenizers()
            .get(&analyzer.tokenizer())
            .ok_or_else(|| {
                EsError::internal(format!("analyzer [{}] introuvable", analyzer.tokenizer()))
            })?;
        let mut stream = analyzer.token_stream(text);
        let mut out = Vec::new();
        while stream.advance() {
            let token = stream.token();
            out.push((token.position, token.text.clone()));
        }
        Ok(out)
    }
}

/// Traduit une requete du DSL. `v` est la valeur de la cle `query`.
pub fn build_query(v: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let q = build_une(v, ctx);
    // Rattrapage au plus pres de la clause : une feuille qui cite un champ que
    // ce mapping ne connait pas devient « ne correspond a rien », et les clauses
    // qui l'entourent (`bool`, `nested`, `dis_max`) continuent de se construire
    // normalement. C'est ce qui fait qu'un `must_not: exists` sur un champ
    // jamais mappe matche tous les documents, comme chez ES, au lieu de faire
    // echouer la recherche entiere.
    match q {
        Err(e) if ctx.champ_inconnu_tolere(&e) => Ok(Box::new(EmptyQuery)),
        autre => autre,
    }
}

fn build_une(v: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(v, "query")?;
    let (name, body) = single_key(obj, "query")?;
    match name {
        "match_all" => match_all(body),
        "match_none" => {
            expect_only(as_object(body, "match_none")?, &[], "match_none")?;
            Ok(Box::new(EmptyQuery))
        }
        "match" => match_query(body, ctx),
        "multi_match" => multi_match_query(body, ctx),
        "match_phrase" => match_phrase_query(body, ctx),
        "match_phrase_prefix" => match_phrase_prefix_query(body, ctx),
        "exists" => exists_query(body, ctx),
        "ids" => ids_query(body, ctx),
        "prefix" => prefix_query(body, ctx),
        "wildcard" => wildcard_query(body, ctx),
        "regexp" => regexp_query(body, ctx),
        "fuzzy" => fuzzy_query(body, ctx),
        "constant_score" => constant_score_query(body, ctx),
        "dis_max" => dis_max_query(body, ctx),
        "function_score" => function_score_query(body, ctx),
        "boosting" => boosting_query(body, ctx),
        "term" => term_query(body, ctx),
        "terms" => terms_query(body, ctx),
        "range" => range_query(body, ctx),
        "bool" => bool_query(body, ctx),
        "nested" => nested_query(body, ctx),
        "has_child" => join_query(body, ctx, Sens::VersLeParent),
        "has_parent" => join_query(body, ctx, Sens::VersLEnfant),
        "parent_id" => parent_id_query(body, ctx),
        other => Err(EsError::parsing(format!(
            "unknown query [{other}] : ferrite supporte [match_all, match_none, match, \
             multi_match, match_phrase, match_phrase_prefix, exists, ids, prefix, wildcard, \
             regexp, fuzzy, term, terms, range, bool, constant_score, dis_max, function_score, \
             boosting, nested, has_child, has_parent, parent_id]"
        ))),
    }
}

/// Une clause posee sur un champ de **metadonnees** (`_id`, `_index`, ...).
///
/// Rend `None` quand le nom n'en est pas un : la clause suit alors son chemin
/// ordinaire. Sinon, [`crate::meta`] a deja tranche entre les trois issues
/// mesurees contre ES — repondre, refuser, ou ne rien designer — et il ne reste
/// qu'a construire la requete.
///
/// C'est ici que se ferme le defaut de la carte 41 : `_id` n'etait dans aucun
/// mapping, la clause tombait dans « champ non mappe », et le defaut d'ES
/// (`allow_unmapped_fields`) la rendait vide **en 200**.
fn meta_query(
    ctx: &QueryCtx,
    champ: &str,
    cl: crate::meta::Clause,
    valeurs: &[Value],
) -> Option<EsResult<Box<dyn Query>>> {
    let verdict = crate::meta::clause(champ, cl, valeurs, ctx.nom_index)?;
    Some(verdict.map(|v| match v {
        // ES donne un score **constant** a une clause sur un champ de
        // metadonnees : 1.0 x `boost`, mesure sur `term`, `terms`, `match` et
        // `exists`. Rien a y noter — il n'y a ni frequence ni longueur de champ.
        crate::meta::Verdict::Tous => {
            Box::new(ConstScoreQuery::new(Box::new(AllQuery), 1.0)) as Box<dyn Query>
        }
        crate::meta::Verdict::Aucun => Box::new(EmptyQuery),
        crate::meta::Verdict::Ids(ids) => {
            let clauses: Vec<(Occur, Box<dyn Query>)> = ids
                .iter()
                .map(|id| {
                    let q: Box<dyn Query> = Box::new(TermQuery::new(
                        tantivy::Term::from_field_text(ctx.fields.id, id),
                        IndexRecordOption::Basic,
                    ));
                    (Occur::Should, q)
                })
                .collect();
            Box::new(ConstScoreQuery::new(
                Box::new(BooleanQuery::new(clauses)),
                1.0,
            ))
        }
    }))
}

/// Le refus qu'Elasticsearch prononce quand une clause vise un champ
/// `index: false` **sans colonne** — c'est-a-dire un `text`.
///
/// Sur tous les autres types, `index: false` ne rend pas le champ inerte : la
/// clause se joue sur la colonne (voir [`crate::colonne`]). Sur un `text`, il
/// n'y a rien sur quoi retomber, et ES refuse avec cette phrase-la (mesure
/// contre 8.15.0).
///
/// Marquee « valeur illisible » pour la meme raison que le refus de
/// `phrase_prefix` sur un `keyword` : c'est une des erreurs que le `lenient`
/// d'ES avale — mesure faite, un `multi_match` `lenient` sur deux champs dont
/// l'un n'est pas indexe rend 200 et cherche dans l'autre.
fn champ_sans_index(champ: &str) -> EsError {
    EsError::new(
        axum::http::StatusCode::BAD_REQUEST,
        "query_shard_exception",
        format!(
            "failed to create query: Cannot search on field [{champ}] since it is not indexed."
        ),
    )
    .sur_valeur_illisible()
}

/// Le refus qu'ES prononce quand une **phrase** est posee sur un champ sans
/// positions. Ce n'est pas le meme message que [`champ_sans_index`] : celui-ci
/// vient de Lucene, l'autre du verificateur de mapping d'ES.
fn phrase_sans_positions(champ: &str) -> EsError {
    EsError::new(
        axum::http::StatusCode::BAD_REQUEST,
        "query_shard_exception",
        format!(
            "failed to create query: field:[{champ}] was indexed without position data; cannot \
             run PhraseQuery"
        ),
    )
    .sur_valeur_illisible()
}

/// La meme clause, mais lue dans la colonne du champ.
fn dans_la_colonne(
    champ: &str,
    ty: mapping::FieldType,
    predicat: PredicatColonne,
) -> Box<dyn Query> {
    Box::new(ColonneQuery::new(champ, ty.kind(), predicat))
}

fn match_all(body: &Value) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "match_all")?;
    expect_only(obj, &["boost"], "match_all")?;
    boost(Box::new(AllQuery), obj.get("boost"))
}

fn match_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "match")?;
    let (field_name, spec) = single_key(obj, "match")?;

    let (query_value, operator, boost_value, lenient) = match spec {
        Value::Object(o) => {
            expect_only(o, &["query", "operator", "boost", "lenient"], "match")?;
            let q = o.get("query").ok_or_else(|| {
                EsError::parsing(format!(
                    "[match] sur [{field_name}] : cle [query] manquante"
                ))
            })?;
            (
                q.clone(),
                read_operator(o.get("operator"), "match")?,
                o.get("boost").cloned(),
                read_lenient(o.get("lenient"))?,
            )
        }
        v => (v.clone(), Occur::Should, None, false),
    };

    let inner = match field_match(field_name, &query_value, operator, "match", ctx) {
        // `lenient` : le champ ne sait pas lire cette valeur — la clause ne
        // correspond a rien, au lieu d'echouer (mesure contre ES 8.15 :
        // `match numero: "alice"` avec `lenient` rend 0 document, sans erreur).
        Err(e) if lenient && e.valeur_illisible => Box::new(EmptyQuery),
        autre => autre?,
    };
    boost(inner, boost_value.as_ref())
}

/// `lenient` : le champ dont le type ne sait pas lire la valeur cherchee est
/// **ecarte** de la clause au lieu de la faire echouer.
///
/// C'est le parametre d'une barre de recherche qui balaie des champs de types
/// differents (`nom` en `text`, `numero` en `long`) : sans lui, taper un nom
/// fait echouer la recherche entiere en 400 parce qu'un des champs vises est
/// numerique. Il ne couvre **que** cette famille d'erreurs (voir
/// [`EsError::valeur_illisible`]) : un parametre non supporte reste un refus.
///
/// ES n'accepte que `true`/`false`, booleen ou chaine — pas `1`, pas `TRUE` —
/// et rend ce message-la sur le reste (mesure contre ES 8.15).
fn read_lenient(value: Option<&Value>) -> EsResult<bool> {
    match value {
        None => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(Value::String(s)) if s == "true" => Ok(true),
        Some(Value::String(s)) if s == "false" => Ok(false),
        Some(v) => {
            let brut = v.as_str().map_or_else(|| v.to_string(), str::to_string);
            Err(EsError::illegal_argument(format!(
                "Failed to parse value [{brut}] as only [true] or [false] are allowed."
            )))
        }
    }
}

/// Les termes analyses, regroupes par **position** : une requete sur un champ
/// `text` est une suite de positions, et chaque position porte une ou plusieurs
/// alternatives.
///
/// La distinction ne se voyait pas tant qu'un analyzer posait un terme par
/// position. Un filtre `ngram` / `edge_ngram` pose **tous** les grammes d'un mot
/// a la position de ce mot : c'est la que « une position » cesse de valoir
/// « un terme », et c'est ce que Lucene appelle une `SynonymQuery`.
fn par_position(field: tantivy::schema::Field, tokens: &[(usize, String)]) -> Vec<Box<dyn Query>> {
    let terme = |t: &str| -> Box<dyn Query> {
        Box::new(TermQuery::new(
            tantivy::Term::from_field_text(field, t),
            IndexRecordOption::WithFreqs,
        ))
    };
    grouper(tokens)
        .into_iter()
        .map(|alternatives| match alternatives.as_slice() {
            [seul] => terme(seul),
            plusieurs => Box::new(BooleanQuery::new(
                plusieurs
                    .iter()
                    .map(|t| (Occur::Should, terme(t)))
                    .collect(),
            )) as Box<dyn Query>,
        })
        .collect()
}

/// Les termes analyses, decoupes en positions — sans rien construire.
fn grouper(tokens: &[(usize, String)]) -> Vec<Vec<&str>> {
    let mut out: Vec<Vec<&str>> = Vec::new();
    let mut precedente = None;
    for (pos, texte) in tokens {
        if precedente == Some(*pos) {
            if let Some(derniere) = out.last_mut() {
                derniere.push(texte);
            }
        } else {
            out.push(vec![texte]);
            precedente = Some(*pos);
        }
    }
    out
}

/// Le coeur de `match` pour **un** champ : analyse la chaine avec l'analyzer du
/// champ, ou compare la valeur telle quelle si le champ n'est pas analyse.
///
/// Partage avec `multi_match`, qui n'est rien d'autre que ce meme travail
/// repete sur plusieurs champs.
fn field_match(
    field_name: &str,
    value: &Value,
    operator: Occur,
    clause: &str,
    ctx: &QueryCtx,
) -> EsResult<Box<dyn Query>> {
    // `match` passe par ici, et `multi_match` avec lui : un champ de
    // metadonnees cite dans `fields` se resout donc au meme endroit que s'il
    // etait seul. Mesure contre ES 8.15 : la valeur n'y est **pas** analysee —
    // `{"match": {"_id": "a b"}}` rend zero document, la ou un `match` analyse
    // en aurait trouve deux.
    if let Some(q) = meta_query(
        ctx,
        field_name,
        crate::meta::Clause::Match,
        std::slice::from_ref(value),
    ) {
        return q;
    }
    let MappedField {
        field,
        ty,
        search_analyzer,
        indexe,
        ..
    } = ctx.field(field_name, clause)?;
    if !indexe {
        // Un `text` non indexe n'a pas de colonne : ES refuse — mais seulement
        // s'il reste un terme a chercher. Une chaine dont l'analyzer ne tire
        // rien (`""`, `"!!!"`) ne construit aucune clause, et rend 200 sans
        // document (mesure contre 8.15).
        if ty.kind() == FieldKind::Text {
            let tokens = ctx.analyze(&query_text(field_name, value, clause)?, search_analyzer)?;
            return if tokens.is_empty() {
                Ok(Box::new(EmptyQuery))
            } else {
                Err(champ_sans_index(field_name))
            };
        }
        // Sur un champ non analyse, `match` se comporte comme `term` : non
        // indexe, il se lit donc dans la colonne, comme `term`.
        return colonne_valeur(field_name, ty, value, ctx);
    }
    Ok(match ty.kind() {
        FieldKind::Text => {
            let tokens = ctx.analyze(&query_text(field_name, value, clause)?, search_analyzer)?;
            // `operator: and` porte sur les **positions**, pas sur les termes.
            // Tant qu'un analyzer posait un terme par position les deux se
            // confondaient ; un filtre a n-grammes en pose dix au meme endroit,
            // et Lucene en fait une union (sa `SynonymQuery`) avant d'appliquer
            // l'operateur. Les exiger tous rendrait « le document contient
            // **tous** les grammes du mot cherche », donc beaucoup moins de
            // documents — en 200 (mesure contre ES 8.15).
            let groupes = par_position(field, &tokens);
            match groupes.len() {
                0 => Box::new(EmptyQuery),
                1 => groupes.into_iter().next().expect("un groupe"),
                _ => Box::new(BooleanQuery::new(
                    groupes.into_iter().map(|q| (operator, q)).collect(),
                )),
            }
        }
        // Sur un champ non analyse, `match` se comporte comme `term` (ES fait
        // pareil : l'analyzer d'un keyword est `keyword`).
        // Une valeur que le type du champ ne sait pas lire est marquee : c'est
        // ce que `lenient` ecarte au lieu de faire echouer la recherche.
        FieldKind::Date => {
            periode_date(field_name, field, value, ctx).map_err(EsError::sur_valeur_illisible)?
        }
        _ => {
            let tv = mapping::coerce_avec(field_name, ty, value, ctx.fields.format_de(field_name))
                .map_err(EsError::sur_valeur_illisible)?;
            let terme: Box<dyn Query> =
                Box::new(TermQuery::new(tv.to_term(field), IndexRecordOption::Basic));
            // Comme dans `term` : sur un numerique, ES interroge un arbre de
            // points et donne le meme score a tout le monde. Sans ce
            // ConstScoreQuery, un `match` sur un champ numerique classait les
            // documents par BM25 — et l'ordre changeait sans rien dire.
            if matches!(ty.kind(), FieldKind::I64 | FieldKind::F64) {
                Box::new(ConstScoreQuery::new(terme, 1.0))
            } else {
                terme
            }
        }
    })
}

fn query_text(field_name: &str, value: &Value, clause: &str) -> EsResult<String> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        v => Err(EsError::illegal_argument(format!(
            "[{clause}] sur [{field_name}] : valeur {v} invalide"
        ))),
    }
}

/// `slop` dans `match_phrase` : seule la phrase exacte est acceptee.
///
/// tantivy et Lucene ne comptent pas les deplacements de la meme facon des que
/// la phrase depasse deux termes : cherchee comme `un deux trois`, la phrase
/// `deux un trois` matche a `slop=2` chez Elasticsearch, et seulement a
/// `slop=3` chez tantivy. Accepter le parametre rendrait donc **moins de
/// documents** qu'ES sur la meme requete, sans que rien ne le signale.
fn read_slop(value: Option<&Value>, clause: &str) -> EsResult<u32> {
    let Some(value) = value else { return Ok(0) };
    let n = value
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| EsError::illegal_argument("[slop] : entier positif attendu"))?;
    if n > 0 {
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas [slop] dans [{clause}] : tantivy et Lucene comptent les \
             deplacements differemment au-dela de deux termes, et le resultat differerait de \
             celui d'Elasticsearch sans que rien ne le signale (voir docs/compat.md). La phrase \
             exacte (slop absent ou 0) est supportee."
        )));
    }
    Ok(0)
}

fn read_operator(value: Option<&Value>, clause: &str) -> EsResult<Occur> {
    match value.and_then(Value::as_str) {
        None => Ok(Occur::Should),
        Some(s) if s.eq_ignore_ascii_case("or") => Ok(Occur::Should),
        Some(s) if s.eq_ignore_ascii_case("and") => Ok(Occur::Must),
        Some(s) => Err(EsError::illegal_argument(format!(
            "[{clause}] : operator [{s}] invalide (or|and)"
        ))),
    }
}

/// `multi_match` : la meme chaine cherchee dans plusieurs champs.
///
/// C'est la clause d'une barre de recherche ordinaire. Quatre strategies de
/// score, celles qui couvrent l'usage courant :
///
/// - `best_fields` (defaut chez ES) : le score du meilleur champ l'emporte
///   (`dis_max`), avec un `tie_breaker` optionnel pour tenir compte des autres.
/// - `most_fields` : les scores de tous les champs s'additionnent.
/// - `phrase` : la meme phrase cherchee dans chaque champ, puis `dis_max` —
///   c'est `match_phrase` repete, exactement comme `best_fields` est `match`
///   repete (mesure contre ES 8.15 : sur deux champs texte, le score du
///   document est celui de son meilleur champ, et `tie_breaker` y ajoute bien
///   0,3 fois l'autre).
/// - `phrase_prefix` : idem avec le dernier mot ampute.
fn multi_match_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "multi_match")?;
    expect_only(
        obj,
        &[
            "query",
            "fields",
            "type",
            "operator",
            "boost",
            "tie_breaker",
            "lenient",
            "slop",
            "max_expansions",
        ],
        "multi_match",
    )?;

    let value = obj
        .get("query")
        .ok_or_else(|| EsError::parsing("[multi_match] : cle [query] manquante"))?;
    let fields = obj.get("fields").and_then(Value::as_array).ok_or_else(|| {
        EsError::illegal_argument(
            "[multi_match] : [fields] est obligatoire et doit etre une liste (ferrite n'a pas de \
             champ de recherche par defaut)",
        )
    })?;
    if fields.is_empty() {
        return Err(EsError::illegal_argument(
            "[multi_match] : [fields] ne peut pas etre vide",
        ));
    }
    let operator = read_operator(obj.get("operator"), "multi_match")?;
    let lenient = read_lenient(obj.get("lenient"))?;
    let ty = obj
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("best_fields");
    match ty {
        "best_fields" | "most_fields" | "phrase" | "phrase_prefix" => {}
        // Deux types qu'ES sait faire et pas ferrite : ils demandent des
        // statistiques de termes fusionnees entre champs (`cross_fields`) ou un
        // scoring de suggestion (`bool_prefix`). Le refus est explicite.
        "cross_fields" | "bool_prefix" => {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [type: {ty}] dans [multi_match] ; types acceptes : \
                 best_fields, most_fields, phrase, phrase_prefix"
            )))
        }
        // Le reste n'existe pas chez ES non plus : son message, mot pour mot.
        _ => {
            return Err(EsError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "parse_exception",
                format!("failed to parse [multi_match] query type [{ty}]. unknown type."),
            ))
        }
    }
    let tie_breaker = match obj.get("tie_breaker") {
        None => 0.0f32,
        Some(v) => v
            .as_f64()
            .ok_or_else(|| EsError::illegal_argument("[tie_breaker] : nombre attendu"))?
            as f32,
    };
    // `most_fields` additionne : il n'y a pas de « meilleur champ » a departager.
    if ty == "most_fields" && obj.contains_key("tie_breaker") {
        return Err(EsError::illegal_argument(
            "[tie_breaker] ne s'applique qu'aux types [best_fields], [phrase] et [phrase_prefix]",
        ));
    }
    // Refuse au-dela de 0, quel que soit le type : tantivy et Lucene ne comptent
    // pas les deplacements pareil (voir `read_slop`).
    let slop = read_slop(obj.get("slop"), "multi_match")?;
    let max_expansions = read_max_expansions(obj.get("max_expansions"))?;

    let mut subs: Vec<Box<dyn Query>> = Vec::with_capacity(fields.len());
    for spec in fields {
        let spec = spec.as_str().ok_or_else(|| {
            EsError::illegal_argument("[multi_match.fields] : liste de chaines attendue")
        })?;
        // Syntaxe d'ES pour ponderer un champ : `titre^3`.
        let (name, field_boost) = match spec.split_once('^') {
            Some((name, b)) => {
                let b = b.trim().parse::<f32>().map_err(|_| {
                    EsError::illegal_argument(format!(
                        "[multi_match.fields] : ponderation invalide dans [{spec}]"
                    ))
                })?;
                (name, Some(b))
            }
            None => (spec, None),
        };
        if name.contains('*') {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas les motifs de champ dans [multi_match.fields] (recu \
                 [{spec}]) : nomme les champs un par un"
            )));
        }
        let construite = match ty {
            "phrase" => field_phrase(name, value, slop, "multi_match", ctx),
            "phrase_prefix" => field_phrase_prefix(name, value, max_expansions, "multi_match", ctx),
            // `operator` ne veut rien dire pour une phrase, et ES l'ignore dans
            // ce cas (mesure) : il n'est lu que par les deux autres types.
            _ => field_match(name, value, operator, "multi_match", ctx),
        };
        let mut q = match construite {
            // Un champ que ce mapping ne connait pas est **ecarte de la liste**,
            // pas fatal a la clause entiere : c'est ce que fait ES, et
            // rattraper plus haut (au niveau de la clause) rendrait 0 document
            // la ou ES en rend, en silence, des que l'un des champs de la barre
            // de recherche n'est pas mappe ici.
            Err(e) if ctx.champ_inconnu_tolere(&e) => continue,
            // `lenient` : ce champ-la ne sait pas lire la valeur cherchee.
            Err(e) if lenient && e.valeur_illisible => continue,
            autre => autre?,
        };
        if let Some(b) = field_boost {
            q = Box::new(BoostQuery::new(q, b));
        }
        subs.push(q);
    }

    // Tous les champs ecartes : la clause ne correspond a rien, sans erreur —
    // et sans matcher non plus sous un `must_not` (mesure contre ES 8.15).
    if subs.is_empty() {
        return boost(Box::new(EmptyQuery), obj.get("boost"));
    }

    let inner: Box<dyn Query> = if ty == "most_fields" {
        Box::new(BooleanQuery::new(
            subs.into_iter().map(|q| (Occur::Should, q)).collect(),
        ))
    } else {
        // `best_fields`, `phrase` et `phrase_prefix` : le meilleur champ
        // l'emporte.
        Box::new(DisMaxQuery::new(subs, tie_breaker))
    };
    boost(inner, obj.get("boost"))
}

/// `max_expansions` : combien de termes un prefixe a le droit de developper.
fn read_max_expansions(value: Option<&Value>) -> EsResult<u32> {
    match value {
        None => Ok(50),
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| EsError::illegal_argument("[max_expansions] : entier positif attendu")),
    }
}

/// `match_phrase` : les termes dans cet ordre, cote a cote.
fn match_phrase_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "match_phrase")?;
    let (field_name, spec) = single_key(obj, "match_phrase")?;

    let (value, slop, boost_value) = match spec {
        Value::Object(o) => {
            expect_only(o, &["query", "slop", "boost"], "match_phrase")?;
            let q = o.get("query").ok_or_else(|| {
                EsError::parsing(format!(
                    "[match_phrase] sur [{field_name}] : cle [query] manquante"
                ))
            })?;
            (
                q.clone(),
                read_slop(o.get("slop"), "match_phrase")?,
                o.get("boost").cloned(),
            )
        }
        v => (v.clone(), 0, None),
    };

    let inner = field_phrase(field_name, &value, slop, "match_phrase", ctx)?;
    boost(inner, boost_value.as_ref())
}

/// Le coeur de `match_phrase` pour **un** champ.
///
/// Partage avec `multi_match` en `type: phrase`, qui n'est rien d'autre que
/// cette phrase repetee sur plusieurs champs, puis mise en `dis_max`.
fn field_phrase(
    field_name: &str,
    value: &Value,
    slop: u32,
    clause: &str,
    ctx: &QueryCtx,
) -> EsResult<Box<dyn Query>> {
    if let Some(q) = meta_query(
        ctx,
        field_name,
        crate::meta::Clause::MatchPhrase,
        std::slice::from_ref(value),
    ) {
        return q;
    }
    let MappedField {
        field,
        ty,
        search_analyzer,
        indexe,
        ..
    } = ctx.field(field_name, clause)?;

    // Une phrase demande des **positions**, et un champ non indexe n'en a pas.
    // Le refus depend alors du **nombre de termes**, parce que c'est Lucene qui
    // parle et pas le verificateur de mapping : a un seul terme il n'y a plus
    // de phrase (c'est un `term`, donc « pas indexe »), a plusieurs c'est la
    // `PhraseQuery` qui manque de positions, et a zero il n'y a pas de clause
    // du tout. Les trois sont mesures contre 8.15.
    if !indexe && ty.kind() == FieldKind::Text {
        let tokens = ctx.analyze(&query_text(field_name, value, clause)?, search_analyzer)?;
        return match grouper(&tokens).len() {
            0 => Ok(Box::new(EmptyQuery)),
            1 => Err(champ_sans_index(field_name)),
            _ => Err(phrase_sans_positions(field_name)),
        };
    }
    if !indexe {
        if slop != 0 {
            return Err(EsError::illegal_argument(format!(
                "[{clause}] : [slop] n'a pas de sens sur le champ non analyse [{field_name}]"
            )));
        }
        return colonne_valeur(field_name, ty, value, ctx);
    }

    let inner: Box<dyn Query> = match ty.kind() {
        FieldKind::Text => {
            let tokens = ctx.analyze(&query_text(field_name, value, clause)?, search_analyzer)?;
            // Une phrase n'est pas une suite de termes, c'est une suite de
            // **positions** — et un analyzer a n-grammes en pose plusieurs a
            // la meme.
            let positions = grouper(&tokens);
            let terme = |t: &str| {
                Box::new(TermQuery::new(
                    tantivy::Term::from_field_text(field, t),
                    IndexRecordOption::WithFreqs,
                )) as Box<dyn Query>
            };
            match positions.as_slice() {
                [] => Box::new(EmptyQuery),
                // Une phrase d'un seul terme est un `term` : tantivy exige au
                // moins deux termes pour une PhraseQuery.
                [seule] if seule.len() == 1 => terme(seule[0]),
                // Plusieurs termes a une **seule** position : ce sont des
                // alternatives, pas une suite. C'est ce que fait Lucene, et
                // c'est le cas courant d'un champ a n-grammes — `match_phrase`
                // y cherche un mot decoupe en grammes, tous poses au meme
                // endroit. Les enchainer rendrait « le document contient
                // exactement cette suite de grammes », c'est-a-dire beaucoup
                // moins de documents, en silence.
                [seule] => Box::new(BooleanQuery::new(
                    seule.iter().map(|t| (Occur::Should, terme(t))).collect(),
                )),
                _ => {
                    if positions.iter().any(|p| p.len() > 1) {
                        return Err(EsError::unsupported(format!(
                            "ferrite ne supporte pas plusieurs termes a la meme position dans \
                             une phrase de plusieurs mots (champ [{field_name}], clause \
                             [{clause}]) : c'est le cas d'un filtre [ngram] ou [edge_ngram], et \
                             Lucene y construit une `MultiPhraseQuery` que tantivy n'a pas. Un \
                             seul mot passe (voir docs/compat.md)"
                        )));
                    }
                    let terms: Vec<(usize, tantivy::Term)> = tokens
                        .iter()
                        .map(|(pos, t)| (*pos, tantivy::Term::from_field_text(field, t)))
                        .collect();
                    Box::new(PhraseQuery::new_with_offset_and_slop(terms, slop))
                }
            }
        }
        // Sur un champ non analyse, une phrase ne peut etre que la valeur
        // entiere — c'est aussi ce que fait ES.
        _ => {
            if slop != 0 {
                return Err(EsError::illegal_argument(format!(
                    "[{clause}] : [slop] n'a pas de sens sur le champ non analyse [{field_name}]"
                )));
            }
            let tv = mapping::coerce_avec(field_name, ty, value, ctx.fields.format_de(field_name))
                .map_err(EsError::sur_valeur_illisible)?;
            Box::new(TermQuery::new(tv.to_term(field), IndexRecordOption::Basic))
        }
    };
    Ok(inner)
}

/// `match_phrase_prefix` : les termes dans cet ordre, le dernier n'etant qu'un
/// debut de mot.
///
/// C'est la clause d'une barre de recherche qui complete pendant la frappe :
/// `"reduction de bru"` doit trouver `reduction de bruit` avant meme que le mot
/// soit fini.
fn match_phrase_prefix_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "match_phrase_prefix")?;
    let (field_name, spec) = single_key(obj, "match_phrase_prefix")?;

    let (value, max_expansions, boost_value) = match spec {
        Value::Object(o) => {
            expect_only(
                o,
                &["query", "slop", "boost", "max_expansions", "analyzer"],
                "match_phrase_prefix",
            )?;
            if o.contains_key("analyzer") {
                return Err(EsError::unsupported(
                    "ferrite ne supporte pas [analyzer] dans [match_phrase_prefix] : la chaine \
                     est analysee avec l'analyzer du champ",
                ));
            }
            read_slop(o.get("slop"), "match_phrase_prefix")?;
            let q = o.get("query").ok_or_else(|| {
                EsError::parsing(format!(
                    "[match_phrase_prefix] sur [{field_name}] : cle [query] manquante"
                ))
            })?;
            let max = read_max_expansions(o.get("max_expansions"))?;
            (q.clone(), max, o.get("boost").cloned())
        }
        v => (v.clone(), 50, None),
    };

    let inner = field_phrase_prefix(
        field_name,
        &value,
        max_expansions,
        "match_phrase_prefix",
        ctx,
    )?;
    boost(inner, boost_value.as_ref())
}

/// Le coeur de `match_phrase_prefix` pour **un** champ.
///
/// Partage avec `multi_match` en `type: phrase_prefix`.
fn field_phrase_prefix(
    field_name: &str,
    value: &Value,
    max_expansions: u32,
    clause: &str,
    ctx: &QueryCtx,
) -> EsResult<Box<dyn Query>> {
    // ES refuse une phrase a prefixe sur **tous** ses champs de metadonnees,
    // `_id` compris : « Can only use phrase prefix queries on text fields ».
    if let Some(q) = meta_query(
        ctx,
        field_name,
        crate::meta::Clause::MatchPhrasePrefix,
        std::slice::from_ref(value),
    ) {
        return q;
    }
    let MappedField {
        field,
        ty,
        search_analyzer,
        indexe,
        ..
    } = ctx.field(field_name, clause)?;

    // Sur un `text` non indexe, le dernier mot se developpe sur un
    // dictionnaire vide : a un seul terme, ES rend donc 200 et aucun document
    // — c'est la seule clause qui ne refuse pas. A plusieurs, il reste une
    // phrase, et il n'y a pas de positions (mesure contre 8.15).
    if !indexe && ty.kind() == FieldKind::Text {
        let tokens = ctx.analyze(&query_text(field_name, value, clause)?, search_analyzer)?;
        return if grouper(&tokens).len() > 1 {
            Err(phrase_sans_positions(field_name))
        } else {
            Ok(Box::new(EmptyQuery))
        };
    }

    let inner: Box<dyn Query> = match ty.kind() {
        FieldKind::Text => {
            let tokens = ctx.analyze(&query_text(field_name, value, clause)?, search_analyzer)?;
            // Plusieurs termes a la meme position : meme regle que dans
            // `field_phrase`. Une **seule** position, ce sont des
            // alternatives, et chacune se developpe par son prefixe ; a
            // plusieurs, il faudrait la `MultiPhraseQuery` que tantivy n'a pas.
            let une_seule_position = tokens.first().map(|t| t.0) == tokens.last().map(|t| t.0);
            if tokens.len() > 1 && une_seule_position {
                let prefixes: Vec<&str> = tokens.iter().map(|(_, t)| t.as_str()).collect();
                let clauses: Vec<(Occur, Box<dyn Query>)> =
                    termes_du_groupe(ctx, field, &prefixes, max_expansions)?
                        .into_iter()
                        .map(|dev| {
                            let q: Box<dyn Query> = Box::new(TermQuery::new(
                                tantivy::Term::from_field_text(field, &dev),
                                IndexRecordOption::WithFreqs,
                            ));
                            (Occur::Should, q)
                        })
                        .collect();
                return Ok(if clauses.is_empty() {
                    Box::new(EmptyQuery)
                } else {
                    Box::new(BooleanQuery::new(clauses))
                });
            }
            if tokens.windows(2).any(|p| p[0].0 == p[1].0) {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas plusieurs termes a la meme position dans un \
                     [{clause}] de plusieurs mots (champ [{field_name}]) : c'est le cas d'un \
                     filtre [ngram] ou [edge_ngram], et Lucene y construit une \
                     `MultiPhraseQuery` que tantivy n'a pas. Un seul mot passe (voir \
                     docs/compat.md)"
                )));
            }
            match tokens.len() {
                0 => Box::new(EmptyQuery),
                // Un seul terme : il n'y a plus de phrase, et Lucene reecrit
                // alors la clause en **disjonction des termes developpes**,
                // chacun score comme un `term` ordinaire. Mesure faite contre
                // ES 8.15 : `match_phrase_prefix [audit]` y rend exactement le
                // score de `match [audit]`. tantivy, lui, donnerait un score
                // constant a ce cas — memes documents, mais dans un ordre qui
                // n'est plus celui d'un moteur de recherche.
                1 => {
                    let clauses: Vec<(Occur, Box<dyn Query>)> =
                        termes_avec_prefixe(ctx, field, &tokens[0].1, max_expansions)?
                            .into_iter()
                            .map(|t| {
                                let q: Box<dyn Query> = Box::new(TermQuery::new(
                                    tantivy::Term::from_field_text(field, &t),
                                    IndexRecordOption::WithFreqs,
                                ));
                                (Occur::Should, q)
                            })
                            .collect();
                    if clauses.is_empty() {
                        Box::new(EmptyQuery)
                    } else {
                        Box::new(BooleanQuery::new(clauses))
                    }
                }
                // Plusieurs termes : une phrase dont le dernier mot est
                // developpe. `PhrasePrefixQuery` de tantivy sait la resoudre,
                // mais **ne la score pas** : son poids BM25 ignore le terme
                // developpe, et rend un score constant (mesure : 1.0 partout,
                // la ou ES classe). Un moteur de recherche qui ne classe pas
                // la clause d'une barre de recherche ne sert a rien, donc le
                // developpement est fait ici : une phrase par terme trouve,
                // scorees comme des phrases — ce qui redonne exactement le
                // score d'ES quand le prefixe ne developpe qu'un terme.
                _ => {
                    let (dernier_pos, dernier) = tokens.last().expect("au moins deux termes");
                    let debut: Vec<(usize, tantivy::Term)> = tokens[..tokens.len() - 1]
                        .iter()
                        .map(|(pos, t)| (*pos, tantivy::Term::from_field_text(field, t)))
                        .collect();
                    let clauses: Vec<(Occur, Box<dyn Query>)> =
                        termes_avec_prefixe(ctx, field, dernier, max_expansions)?
                            .into_iter()
                            .map(|t| {
                                let mut termes = debut.clone();
                                termes.push((
                                    *dernier_pos,
                                    tantivy::Term::from_field_text(field, &t),
                                ));
                                let q: Box<dyn Query> =
                                    Box::new(PhraseQuery::new_with_offset(termes));
                                (Occur::Should, q)
                            })
                            .collect();
                    if clauses.is_empty() {
                        Box::new(EmptyQuery)
                    } else {
                        Box::new(BooleanQuery::new(clauses))
                    }
                }
            }
        }
        // Une phrase suppose des positions, qu'un champ non analyse n'a pas.
        // ES refuse ici, avec ce message : ferrite le reprend mot pour mot,
        // pour qu'un client ne voie pas la difference. Marquee « valeur
        // illisible » parce que c'est aussi une des erreurs que le `lenient`
        // d'ES avale (mesure : `phrase_prefix` sur un `keyword` rend 0 document
        // au lieu d'echouer).
        _ => {
            return Err(EsError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "query_shard_exception",
                format!(
                    "failed to create query: Can only use phrase prefix queries on text fields - \
                     not on [{field_name}] which is of type [{}]",
                    ty.name()
                ),
            )
            .sur_valeur_illisible())
        }
    };
    Ok(inner)
}

/// Les termes indexes qui commencent par ce prefixe, dans l'ordre du
/// dictionnaire et bornes par `max`.
///
/// C'est le developpement que fait Lucene avant de scorer : le prendre au
/// dictionnaire de termes, segment par segment, coute le parcours d'une plage
/// et rien de plus.
fn termes_avec_prefixe(
    ctx: &QueryCtx,
    field: tantivy::schema::Field,
    prefixe: &str,
    max: u32,
) -> EsResult<Vec<String>> {
    termes_du_groupe(ctx, field, std::slice::from_ref(&prefixe), max)
}

/// Le meme developpement pour **tous les termes d'une position**, avec un
/// budget commun.
///
/// `max_expansions` est chez Lucene un budget **par position**, pas par terme :
/// `MultiPhrasePrefixQuery` remplit un seul ensemble en parcourant les termes de
/// la position dans l'ordre de l'analyzer, et s'arrete des qu'il est plein. La
/// distinction ne se voyait pas tant qu'un analyzer posait un terme par
/// position ; un filtre a n-grammes en pose vingt, et un budget par terme en
/// developpe alors vingt fois plus. ferrite rendait **un document de plus**
/// qu'ES sur un `match_phrase_prefix` d'un seul mot — en 200 (trouve par une
/// plage de controle du fuzzer, graine 4242075).
fn termes_du_groupe(
    ctx: &QueryCtx,
    field: tantivy::schema::Field,
    prefixes: &[&str],
    max: u32,
) -> EsResult<Vec<String>> {
    let mut trouves = std::collections::BTreeSet::new();
    for prefixe in prefixes {
        for segment in ctx.searcher.segment_readers() {
            let inverse = segment
                .inverted_index(field)
                .map_err(|e| EsError::internal(format!("index inverse illisible : {e}")))?;
            let mut flux = inverse
                .terms()
                .range()
                .ge(prefixe.as_bytes())
                .into_stream()
                .map_err(|e| {
                    EsError::internal(format!("dictionnaire de termes illisible : {e}"))
                })?;
            while flux.advance() {
                // Le budget est commun : il se lit sur l'ensemble deja rempli,
                // pas sur ce que ce prefixe-ci a pris.
                if trouves.len() as u32 >= max {
                    break;
                }
                let Ok(terme) = std::str::from_utf8(flux.key()) else {
                    continue;
                };
                if !terme.starts_with(*prefixe) {
                    break;
                }
                trouves.insert(terme.to_string());
            }
        }
        if trouves.len() as u32 >= max {
            break;
        }
    }
    Ok(trouves.into_iter().collect())
}

/// `exists` : les documents qui ont au moins une valeur pour ce champ.
fn exists_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "exists")?;
    expect_only(obj, &["field", "boost"], "exists")?;
    let name = obj
        .get("field")
        .and_then(Value::as_str)
        .ok_or_else(|| EsError::illegal_argument("[exists] : cle [field] manquante"))?;
    if let Some(q) = meta_query(ctx, name, crate::meta::Clause::Exists, &[]) {
        // `exists` est la clause ou les champs de metadonnees se separent le
        // plus : tous les documents en portent un (`_id`, `_index`, `_seq_no`,
        // `_version`), aucun n'a de `_type`, et ES **refuse** la question sur
        // `_field_names` et `_source`. Mesure, pas deduction.
        return boost(
            Box::new(ConstScoreQuery::new(q?, 1.0)) as Box<dyn Query>,
            obj.get("boost"),
        );
    }
    let MappedField {
        field, ty, indexe, ..
    } = ctx.field(name, "exists")?;

    let inner: Box<dyn Query> = match ty.kind() {
        // Un `text` non indexe n'a ni index inverse ni colonne : ES y rend 200
        // et **aucun** document, sans erreur (mesure : `exists` y est le seul
        // qui ne refuse pas). Il y construit un `FieldExistsQuery` sur un champ
        // dont rien ne temoigne.
        FieldKind::Text if !indexe => Box::new(EmptyQuery),
        // Les champs `text` n'ont pas de fast field (ce serait doubler le
        // stockage du texte) : « avoir une valeur » s'y lit dans l'index
        // inverse, comme « avoir au moins un terme ».
        FieldKind::Text => Box::new(RangeQuery::new(
            Bound::Included(tantivy::Term::from_field_text(field, "")),
            Bound::Unbounded,
        )),
        _ => Box::new(ExistsQuery::new(name.to_string(), false)),
    };
    // ES donne un score constant a `exists`.
    boost(Box::new(ConstScoreQuery::new(inner, 1.0)), obj.get("boost"))
}

fn term_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "term")?;
    let (field_name, spec) = single_key(obj, "term")?;
    // Meme ordre que dans [range] : le refus de [case_insensitive] doit sortir
    // meme si le champ n'est pas mappe.
    let (value, boost_value) = match spec {
        Value::Object(o) => {
            expect_only(o, &["value", "boost", "case_insensitive"], "term")?;
            if o.contains_key("case_insensitive") {
                return Err(EsError::unsupported(
                    "ferrite ne supporte pas [case_insensitive] sur [term]",
                ));
            }
            let v = o.get("value").ok_or_else(|| {
                EsError::parsing(format!("[term] sur [{field_name}] : cle [value] manquante"))
            })?;
            (v.clone(), o.get("boost").cloned())
        }
        v => (v.clone(), None),
    };
    if let Some(q) = meta_query(
        ctx,
        field_name,
        crate::meta::Clause::Term,
        std::slice::from_ref(&value),
    ) {
        return boost(q?, boost_value.as_ref());
    }
    let MappedField {
        field, ty, indexe, ..
    } = ctx.field(field_name, "term")?;

    if !indexe {
        return boost(
            colonne_valeur(field_name, ty, &value, ctx)?,
            boost_value.as_ref(),
        );
    }
    if ty.kind() == FieldKind::Date {
        return boost(
            periode_date(field_name, field, &value, ctx)?,
            boost_value.as_ref(),
        );
    }
    let tv = mapping::coerce_avec(field_name, ty, &value, ctx.fields.format_de(field_name))?;
    let record = if ty.kind() == FieldKind::Text {
        IndexRecordOption::WithFreqs
    } else {
        IndexRecordOption::Basic
    };
    let terme: Box<dyn Query> = Box::new(TermQuery::new(tv.to_term(field), record));
    // Sur un champ numerique, ES n'interroge pas l'index inverse mais un arbre
    // de points, et donne a tout le monde le **meme** score (1.0 × boost).
    // tantivy, lui, notait par BM25 : un document dont le champ portait
    // plusieurs valeurs marquait moins qu'un autre, et le classement changeait
    // sans que rien ne le signale. Mesure : `{"term": {"n": 5}}` rendait
    // 0.562 / 0.354 la ou ES rend 1.0 / 1.0.
    let interne: Box<dyn Query> = if matches!(ty.kind(), FieldKind::I64 | FieldKind::F64) {
        Box::new(ConstScoreQuery::new(terme, 1.0))
    } else {
        terme
    };
    boost(interne, boost_value.as_ref())
}

/// Une valeur exacte cherchee dans la **colonne** d'un champ `index: false`.
///
/// Le decoupage est celui de l'index inverse — une date designe une periode,
/// tout le reste une valeur — pour que les deux chemins rendent les memes
/// documents.
fn colonne_valeur(
    field_name: &str,
    ty: mapping::FieldType,
    value: &Value,
    ctx: &QueryCtx,
) -> EsResult<Box<dyn Query>> {
    if ty.kind() == FieldKind::Text {
        return Err(champ_sans_index(field_name));
    }
    let predicat = if ty.kind() == FieldKind::Date {
        let (bas, haut) = periode_ms(field_name, value, ctx)?;
        PredicatColonne::Intervalle {
            bas: Bound::Included(TypedValue::Date(bas)),
            haut: Bound::Included(TypedValue::Date(haut)),
        }
    } else {
        PredicatColonne::Valeurs(vec![mapping::coerce_avec(
            field_name,
            ty,
            value,
            ctx.fields.format_de(field_name),
        )
        .map_err(EsError::sur_valeur_illisible)?])
    };
    Ok(dans_la_colonne(field_name, ty, predicat))
}

/// Les deux instants qu'une valeur de date designe : le premier et le dernier
/// de la periode qu'elle couvre.
fn periode_ms(field_name: &str, v: &Value, ctx: &QueryCtx) -> EsResult<(i64, i64)> {
    let format = ctx.fields.format_ou_defaut(field_name);
    Ok((
        datemath::borne(v, format, ctx.maintenant, Arrondi::Bas)?,
        datemath::borne(v, format, ctx.maintenant, Arrondi::Haut)?,
    ))
}

/// Ce qu'une valeur de date designe hors d'un `range` (`term`, `terms`,
/// `match`) : la **periode** qu'elle couvre, pas un instant.
///
/// `{"term": {"d": "2026-03-15"}}` rend chez ES tous les documents du 15 mars,
/// pas seulement ceux de minuit pile (mesure). Le date math y est accepte de la
/// meme facon : `{"term": {"d": "now/d"}}`, c'est « aujourd'hui ».
fn periode_date(
    field_name: &str,
    field: tantivy::schema::Field,
    v: &Value,
    ctx: &QueryCtx,
) -> EsResult<Box<dyn Query>> {
    let (bas, haut) = periode_ms(field_name, v, ctx)?;
    Ok(Box::new(RangeQuery::new(
        Bound::Included(TypedValue::Date(bas).to_term(field)),
        Bound::Included(TypedValue::Date(haut).to_term(field)),
    )))
}

fn terms_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "terms")?;
    let mut boost_value = None;
    let mut entry: Option<(&String, &Value)> = None;
    for (k, v) in obj {
        if k == "boost" {
            boost_value = Some(v.clone());
        } else if entry.is_some() {
            return Err(EsError::parsing(
                "[terms] n'accepte qu'un seul champ par clause",
            ));
        } else {
            entry = Some((k, v));
        }
    }
    let (field_name, values) =
        entry.ok_or_else(|| EsError::parsing("[terms] : aucun champ fourni"))?;
    let list = values.as_array().ok_or_else(|| {
        EsError::illegal_argument(format!(
            "[terms] sur [{field_name}] : une liste de valeurs est attendue (les lookups de \
             termes ne sont pas supportes par ferrite)"
        ))
    })?;
    if let Some(q) = meta_query(ctx, field_name, crate::meta::Clause::Terms, list) {
        return boost(q?, boost_value.as_ref());
    }
    let MappedField {
        field, ty, indexe, ..
    } = ctx.field(field_name, "terms")?;

    // Sur un champ non indexe, chaque valeur devient une lecture de colonne, et
    // l'union se fait comme au-dessus : le `terms` d'ES est un `should` de
    // `term`, la ou qu'il lise.
    if !indexe {
        let clauses: Vec<(Occur, Box<dyn Query>)> = list
            .iter()
            .map(|v| Ok((Occur::Should, colonne_valeur(field_name, ty, v, ctx)?)))
            .collect::<EsResult<_>>()?;
        return boost(
            Box::new(ConstScoreQuery::new(
                Box::new(BooleanQuery::new(clauses)),
                1.0,
            )),
            boost_value.as_ref(),
        );
    }

    let clauses: Vec<(Occur, Box<dyn Query>)> = list
        .iter()
        .map(|v| {
            if ty.kind() == FieldKind::Date {
                return Ok((Occur::Should, periode_date(field_name, field, v, ctx)?));
            }
            let tv = mapping::coerce_avec(field_name, ty, v, ctx.fields.format_de(field_name))?;
            let q: Box<dyn Query> =
                Box::new(TermQuery::new(tv.to_term(field), IndexRecordOption::Basic));
            Ok((Occur::Should, q))
        })
        .collect::<EsResult<_>>()?;

    // ES donne un score constant aux clauses `terms`.
    let inner: Box<dyn Query> = Box::new(ConstScoreQuery::new(
        Box::new(BooleanQuery::new(clauses)),
        1.0,
    ));
    boost(inner, boost_value.as_ref())
}

/// Le `format` d'une clause, s'il en fournit un : chez ES il remplace celui du
/// mapping pour **lire les bornes** d'une requete, et lui seul (une reponse
/// reste rendue au format du champ).
///
/// Il ne s'applique pas a `now` : `now` n'est pas une date ecrite.
fn format_de_requete<'a>(
    v: Option<&Value>,
    champ: &str,
    ctx: &'a QueryCtx,
) -> EsResult<std::borrow::Cow<'a, DateFormat>> {
    match v {
        None | Some(Value::Null) => Ok(std::borrow::Cow::Borrowed(
            ctx.fields.format_ou_defaut(champ),
        )),
        Some(Value::String(s)) => Ok(std::borrow::Cow::Owned(DateFormat::parse(s)?)),
        Some(autre) => Err(EsError::illegal_argument(format!(
            "[format] sur [{champ}] : une chaine est attendue, pas {autre}"
        ))),
    }
}

fn range_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "range")?;
    let (field_name, spec) = single_key(obj, "range")?;
    let spec = as_object(spec, "range")?;
    // Les parametres se lisent **avant** le champ : sinon un champ non mappe
    // (tolere par `allow_unmapped_fields`) escamote le refus de [time_zone] ou
    // [relation], et la clause serait acceptee en silence.
    expect_only(
        spec,
        &["gte", "gt", "lte", "lt", "boost", "format", "time_zone"],
        "range",
    )?;
    // Le fuseau se lit avant le champ pour la meme raison : un fuseau inconnu
    // doit etre refuse meme si le champ ne l'est pas.
    let fuseau = match spec.get("time_zone") {
        None | Some(Value::Null) => crate::fuseau::Fuseau::utc(),
        Some(Value::String(s)) => crate::fuseau::Fuseau::parse(s)?,
        Some(autre) => {
            return Err(EsError::illegal_argument(format!(
                "[range.time_zone] : une chaine est attendue, pas {autre}"
            )))
        }
    };
    // ES refuse un `range` sur **chacun** de ses champs de metadonnees sauf
    // `_seq_no` et `_version` (mesure) : « Field [_id] of type [_id] does not
    // support range queries ».
    if let Some(q) = meta_query(ctx, field_name, crate::meta::Clause::Range, &[]) {
        return boost(q?, spec.get("boost"));
    }
    let MappedField {
        field, ty, indexe, ..
    } = ctx.field(field_name, "range")?;
    // Sur un champ qui n'est pas une date, `time_zone` ne veut rien dire — et
    // ES l'accepte quand meme, en 200, sans rien en faire (mesure contre ES
    // 8.15 : `range` sur un `long` avec un `time_zone` rend les memes
    // documents). Le refuser ici ferait echouer une requete qu'une instance
    // reelle sert : c'est le meme genre de demande vide que le `"index": true`
    // d'un generateur de mapping.

    if ty.kind() == FieldKind::Text {
        // Non indexe, c'est ES lui-meme qui refuse, et sa phrase nomme la vraie
        // raison : il n'y a plus rien a interroger.
        if !indexe {
            return Err(champ_sans_index(field_name));
        }
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas [range] sur un champ [text] (champ [{field_name}]) ; \
             utilise un champ [keyword]"
        )));
    }
    let format = format_de_requete(spec.get("format"), field_name, ctx)?;

    let to_valeur = |key: &str| -> EsResult<Option<TypedValue>> {
        match spec.get(key) {
            None | Some(Value::Null) => Ok(None),
            // Sur une date, la borne n'est pas une valeur mais une
            // **expression** (`now-1d/d`), et une date moins precise que la
            // milliseconde couvre une periode : `gte` et `lt` en prennent le
            // premier instant, `gt` et `lte` le dernier (voir
            // [`crate::datemath`], tout y est mesure contre ES).
            Some(v) if ty.kind() == FieldKind::Date => {
                let arrondi = match key {
                    "gte" | "lt" => Arrondi::Bas,
                    _ => Arrondi::Haut,
                };
                let ms =
                    datemath::borne_dans(v, format.as_ref(), ctx.maintenant, arrondi, &fuseau)?;
                Ok(Some(TypedValue::Date(ms)))
            }
            Some(v) => Ok(Some(mapping::coerce_avec(
                field_name,
                ty,
                v,
                ctx.fields.format_de(field_name),
            )?)),
        }
    };

    let lower = match (to_valeur("gte")?, to_valeur("gt")?) {
        (Some(_), Some(_)) => {
            return Err(EsError::illegal_argument(
                "[range] : [gte] et [gt] sont mutuellement exclusifs",
            ))
        }
        (Some(t), None) => Bound::Included(t),
        (None, Some(t)) => Bound::Excluded(t),
        (None, None) => Bound::Unbounded,
    };
    let upper = match (to_valeur("lte")?, to_valeur("lt")?) {
        (Some(_), Some(_)) => {
            return Err(EsError::illegal_argument(
                "[range] : [lte] et [lt] sont mutuellement exclusifs",
            ))
        }
        (Some(t), None) => Bound::Included(t),
        (None, Some(t)) => Bound::Excluded(t),
        (None, None) => Bound::Unbounded,
    };
    if matches!(lower, Bound::Unbounded) && matches!(upper, Bound::Unbounded) {
        return Err(EsError::illegal_argument(format!(
            "[range] sur [{field_name}] : au moins une borne (gte/gt/lte/lt) est requise"
        )));
    }

    // Sur un champ non indexe, l'intervalle se lit dans la colonne — y compris
    // sur un `boolean`, ou il n'a plus besoin d'etre enumere.
    if !indexe {
        // Un bord d'ES qu'aucune documentation ne donne, et qui ne vaut que
        // la : sur un `boolean` **non indexe**, un `lt` efface le reste de
        // l'intervalle. La table des 24 combinaisons, mesuree contre 8.15.0,
        // ne laisse pas d'autre lecture — des que `lt` est present, la reponse
        // est « toutes les valeurs <= la borne », **borne basse comprise** :
        //
        //     {"lt": false}                  -> false
        //     {"lt": true}                   -> false, true
        //     {"gt": true, "lt": false}      -> false        (!)
        //     {"gte": true, "lt": true}      -> false, true  (!)
        //     {"gt": false, "lte": true}     -> true         (avec `lte`, correct)
        //     {"gte": true, "lte": false}    -> rien         (avec `lte`, correct)
        //
        // Les entiers, les flottants, les dates et les chaines n'ont pas ce
        // bord (mesure type par type) : il est propre au booleen sans index.
        // Ca ressemble a un defaut d'Elasticsearch, et c'est quand meme ce que
        // ferrite rend — rendre **moins** de documents que lui, en silence, est
        // le resultat que ce depot refuse en premier. Trouve par le fuzzer,
        // graine 5550060, et fige dans `sonde_index_false.py`.
        let (bas, haut) = match (ty.kind(), &upper) {
            (FieldKind::Bool, Bound::Excluded(v)) => (Bound::Unbounded, Bound::Included(v.clone())),
            _ => (lower, upper),
        };
        return boost(
            dans_la_colonne(field_name, ty, PredicatColonne::Intervalle { bas, haut }),
            spec.get("boost"),
        );
    }

    let vers_terme = |b: &Bound<TypedValue>| match b {
        Bound::Unbounded => Bound::Unbounded,
        Bound::Included(v) => Bound::Included(v.to_term(field)),
        Bound::Excluded(v) => Bound::Excluded(v.to_term(field)),
    };

    // Un booleen n'a que deux valeurs : le `RangeQuery` de tantivy refuse d'en
    // faire un intervalle (« Expected term with u64, i64, f64 or date ») et
    // rendait un 500. On enumere donc les valeurs que les bornes laissent
    // passer, ce qui est exactement le sens du `range` d'ES sur un `boolean` —
    // et les deux valeurs a la fois veut dire « le champ a une valeur ».
    if ty.kind() == FieldKind::Bool {
        let retenues: Vec<bool> = [false, true]
            .into_iter()
            .filter(|v| {
                let t = TypedValue::Bool(*v);
                crate::colonne::borne_basse(&t, &lower) && crate::colonne::borne_haute(&t, &upper)
            })
            .collect();
        let interne: Box<dyn Query> = match retenues.as_slice() {
            [] => Box::new(tantivy::query::EmptyQuery),
            [v] => Box::new(TermQuery::new(
                TypedValue::Bool(*v).to_term(field),
                IndexRecordOption::Basic,
            )),
            _ => Box::new(ExistsQuery::new(field_name.to_string(), false)),
        };
        return boost(
            Box::new(ConstScoreQuery::new(interne, 1.0)),
            spec.get("boost"),
        );
    }

    let inner: Box<dyn Query> = Box::new(ConstScoreQuery::new(
        Box::new(RangeQuery::new(vers_terme(&lower), vers_terme(&upper))),
        1.0,
    ));
    boost(inner, spec.get("boost"))
}

/// L'ecriture camelCase que `bool` accepte **encore** chez Elasticsearch.
///
/// C'est le seul alias de ce genre que la 8.15 serve : `minimumShouldMatch`,
/// `adjustPureNegative`, `maxExpansions`, `caseInsensitive`, `tieBreaker`,
/// `scoreMode` sont tous refuses (mesure, un par un). Le refuser ici n'etait
/// donc pas un manque de ferrite mais un refus de trop, du meme genre que
/// l'`index: true` de Gitea — et il rendait Wagtail inutilisable, qui l'ecrit
/// sur chacune de ses negations.
pub(crate) const MUST_NOT_CAMEL: &str = "mustNot";

fn bool_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "bool")?;
    expect_only(
        obj,
        &[
            "must",
            "should",
            "filter",
            "must_not",
            MUST_NOT_CAMEL,
            "minimum_should_match",
            "boost",
        ],
        "bool",
    )?;
    if obj.contains_key("must_not") && obj.contains_key(MUST_NOT_CAMEL) {
        return Err(EsError::parsing(
            "[bool] : [must_not] et [mustNot] sont deux ecritures du meme parametre",
        ));
    }

    let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
    let mut should_count = 0usize;
    let mut has_required = false;

    for (key, occur) in [
        ("must", Occur::Must),
        ("filter", Occur::Must),
        ("should", Occur::Should),
        ("must_not", Occur::MustNot),
        (MUST_NOT_CAMEL, Occur::MustNot),
    ] {
        let Some(v) = obj.get(key) else { continue };
        let list: Vec<&Value> = match v {
            Value::Array(a) => a.iter().collect(),
            Value::Null => continue,
            other => vec![other],
        };
        for sub in list {
            let mut q = build_query(sub, ctx)?;
            if key == "filter" {
                // Contexte filtre : le score ne doit pas bouger.
                q = Box::new(ConstScoreQuery::new(q, 0.0));
            }
            if occur == Occur::Should {
                should_count += 1;
            } else if occur == Occur::Must {
                has_required = true;
            }
            clauses.push((occur, q));
        }
    }

    if clauses.is_empty() {
        return boost(Box::new(AllQuery), obj.get("boost"));
    }

    // Un `bool` qui n'a que des `must_not` ne matche rien chez tantivy, alors
    // qu'ES l'interprete comme « tous les documents, sauf ceux-la ». On pose la
    // clause positive implicite.
    //
    // Elle ne **note** rien : ES donne `0.0` a ces documents, quel que soit le
    // `boost` du `bool` (mesure). ferrite leur donnait le score de la clause
    // implicite — `1.5` sous un `boost: 1.5` — et l'ordre changeait des que ce
    // `bool` etait combine a autre chose, sans que rien ne le signale.
    let purement_negatif = !has_required && should_count == 0;
    if purement_negatif {
        clauses.insert(0, (Occur::Must, Box::new(AllQuery)));
        has_required = true;
    }

    // Semantique ES : sans clause obligatoire, au moins un `should` doit matcher.
    let defaut = usize::from(!has_required && should_count > 0);
    let min_should = crate::msm::resoudre(obj.get("minimum_should_match"), should_count, defaut)?;
    if min_should > should_count {
        return Ok(Box::new(EmptyQuery));
    }

    let mut inner: Box<dyn Query> = if min_should > 0 {
        Box::new(BooleanQuery::with_minimum_required_clauses(
            clauses, min_should,
        ))
    } else {
        Box::new(BooleanQuery::new(clauses))
    };
    if purement_negatif {
        inner = Box::new(ConstScoreQuery::new(inner, 0.0));
    }
    boost(inner, obj.get("boost"))
}

/// `ids` : les documents dont l'identifiant figure dans la liste.
fn ids_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "ids")?;
    expect_only(obj, &["values", "boost"], "ids")?;
    let values = obj
        .get("values")
        .and_then(Value::as_array)
        .ok_or_else(|| EsError::illegal_argument("[ids] : [values] (liste) est obligatoire"))?;

    let clauses: Vec<(Occur, Box<dyn Query>)> = values
        .iter()
        .map(|v| {
            let id = v
                .as_str()
                .ok_or_else(|| EsError::illegal_argument("[ids.values] : chaines attendues"))?;
            let q: Box<dyn Query> = Box::new(TermQuery::new(
                tantivy::Term::from_field_text(ctx.fields.id, id),
                IndexRecordOption::Basic,
            ));
            Ok((Occur::Should, q))
        })
        .collect::<EsResult<_>>()?;
    boost(
        Box::new(ConstScoreQuery::new(
            Box::new(BooleanQuery::new(clauses)),
            1.0,
        )),
        obj.get("boost"),
    )
}

/// Lit la forme courte (`{"champ": "valeur"}`) ou longue
/// (`{"champ": {"value": ..., "boost": ...}}`) d'une clause a un champ.
fn valeur_et_boost<'a>(
    obj: &'a Map<String, Value>,
    clause: &str,
    extra: &[&str],
) -> EsResult<(&'a str, String, Option<Value>)> {
    let (champ, spec) = single_key(obj, clause)?;
    match spec {
        Value::Object(o) => {
            let mut permis = vec!["value", "boost"];
            permis.extend_from_slice(extra);
            expect_only(o, &permis, clause)?;
            let v = o.get("value").ok_or_else(|| {
                EsError::parsing(format!("[{clause}] sur [{champ}] : cle [value] manquante"))
            })?;
            Ok((
                champ,
                query_text(champ, v, clause)?,
                o.get("boost").cloned(),
            ))
        }
        v => Ok((champ, query_text(champ, v, clause)?, None)),
    }
}

/// Un champ interrogeable par motif doit etre non analyse : sur un `text`, ES
/// compare au **terme** indexe, ce qui surprend plus souvent que ca n'aide.
fn champ_de_motif(ctx: &QueryCtx, champ: &str, clause: &str) -> EsResult<MappedField> {
    let mf = ctx.field(champ, clause)?;
    if mf.ty.kind() != FieldKind::Keyword && mf.ty.kind() != FieldKind::Text {
        return Err(EsError::illegal_argument(format!(
            "[{clause}] ne s'applique qu'a un champ [text] ou [keyword] ; [{champ}] est de type \
             [{}]",
            mf.ty.name()
        )));
    }
    Ok(mf)
}

/// Une clause de motif posee sur un champ `index: false`.
///
/// Le motif est deja traduit — c'est le meme que celui de l'index inverse — et
/// il est ici confronte au dictionnaire de la colonne. Sur un `text`, il n'y a
/// pas de colonne : c'est le refus d'ES.
fn motif_dans_la_colonne(
    champ: &str,
    mf: &MappedField,
    motif: &str,
    clause: &str,
) -> EsResult<Box<dyn Query>> {
    if mf.ty.kind() == FieldKind::Text {
        return Err(champ_sans_index(champ));
    }
    let regex = tantivy_fst::Regex::new(motif)
        .map_err(|e| EsError::illegal_argument(format!("[{clause}] : {e}")))?;
    Ok(dans_la_colonne(
        champ,
        mf.ty,
        PredicatColonne::Automate(Automate::Regex(regex)),
    ))
}

/// `prefix` : les termes qui commencent par cette chaine. Non analysee, comme
/// chez ES.
fn prefix_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "prefix")?;
    let (champ, valeur, boost_value) =
        valeur_et_boost(obj, "prefix", &["case_insensitive", "rewrite"])?;
    refuser_rewrite(obj, "prefix")?;
    let insensible = lire_insensible(obj, "prefix")?;
    if let Some(q) = meta_query(
        ctx,
        champ,
        crate::meta::Clause::Prefix,
        std::slice::from_ref(&Value::String(valeur.clone())),
    ) {
        return boost(q?, boost_value.as_ref());
    }
    let mf = champ_de_motif(ctx, champ, "prefix")?;
    let motif = format!(
        "(?s){}(?s:.*)",
        crate::regexp::litteral(&valeur, insensible)
    );
    if !mf.indexe {
        return boost(
            motif_dans_la_colonne(champ, &mf, &motif, "prefix")?,
            boost_value.as_ref(),
        );
    }
    let q = RegexQuery::from_pattern(&motif, mf.field)
        .map_err(|e| EsError::illegal_argument(format!("[prefix] : {e}")))?;
    boost(
        Box::new(ConstScoreQuery::new(Box::new(q), 1.0)),
        boost_value.as_ref(),
    )
}

/// `wildcard` : `*` remplace n'importe quelle suite, `?` un seul caractere, et
/// `\` echappe le caractere suivant.
fn wildcard_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "wildcard")?;
    let (champ, valeur, boost_value) = valeur_et_boost(
        obj,
        "wildcard",
        &["case_insensitive", "rewrite", "wildcard"],
    )?;
    refuser_rewrite(obj, "wildcard")?;
    let insensible = lire_insensible(obj, "wildcard")?;
    if let Some(q) = meta_query(
        ctx,
        champ,
        crate::meta::Clause::Wildcard,
        std::slice::from_ref(&Value::String(valeur.clone())),
    ) {
        return boost(q?, boost_value.as_ref());
    }
    let mf = champ_de_motif(ctx, champ, "wildcard")?;

    let motif = format!("(?s){}", crate::regexp::joker(&valeur, insensible));
    if !mf.indexe {
        return boost(
            motif_dans_la_colonne(champ, &mf, &motif, "wildcard")?,
            boost_value.as_ref(),
        );
    }
    let q = RegexQuery::from_pattern(&motif, mf.field)
        .map_err(|e| EsError::illegal_argument(format!("[wildcard] : {e}")))?;
    boost(
        Box::new(ConstScoreQuery::new(Box::new(q), 1.0)),
        boost_value.as_ref(),
    )
}

/// `regexp` : le motif est confronte aux **termes** indexes, ancre des deux
/// cotes.
///
/// C'est la clause qu'un service ecrit pour ses filtres « contient / commence
/// par / finit par », souvent insensibles a la casse. La syntaxe est celle de
/// Lucene, pas celle du crate `regex` : elle est traduite par [`crate::regexp`],
/// qui refuse ce qu'un automate ne sait pas construire plutot que de le prendre
/// pour un litteral (voir le module pour le detail des divergences).
fn regexp_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "regexp")?;
    let (champ, valeur, boost_value) = valeur_et_boost(
        obj,
        "regexp",
        &[
            "case_insensitive",
            "flags",
            "rewrite",
            "max_determinized_states",
        ],
    )?;
    refuser_rewrite(obj, "regexp")?;
    let insensible = lire_insensible(obj, "regexp")?;
    let spec = single_key(obj, "regexp")?.1;
    let params = spec.as_object();

    // `max_determinized_states` borne la taille de l'automate chez Lucene.
    // tantivy a sa propre borne, mais elle ne compte pas la meme chose :
    // l'accepter en le laissant sans effet serait promettre une protection qui
    // n'existe pas.
    if params.is_some_and(|o| o.contains_key("max_determinized_states")) {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas [max_determinized_states] dans [regexp] : tantivy borne \
             l'automate autrement, et accepter le parametre sans l'appliquer promettrait une \
             protection qui n'existe pas",
        ));
    }
    let flags = match params.and_then(|o| o.get("flags")) {
        None => crate::regexp::Flags::default(),
        Some(Value::String(s)) => crate::regexp::Flags::lire(s)?,
        Some(_) => {
            return Err(EsError::illegal_argument(
                "[regexp] : [flags] attend une chaine (par exemple \"COMPLEMENT|INTERVAL\")",
            ))
        }
    };

    // Le motif se traduit avant que le champ soit resolu : les operateurs que
    // ferrite refuse (`~`, `&`, `<n-m>`, `#`) doivent l'etre aussi sur un champ
    // non mappe, sans quoi la clause passerait en silence.
    let motif = crate::regexp::vers_regex(&valeur, flags, insensible)?;
    if let Some(q) = meta_query(
        ctx,
        champ,
        crate::meta::Clause::Regexp,
        std::slice::from_ref(&Value::String(valeur.clone())),
    ) {
        return boost(q?, boost_value.as_ref());
    }
    let mf = champ_de_motif(ctx, champ, "regexp")?;
    if !mf.indexe {
        // Le seul motif qu'ES **refuse** sur une colonne : son automate de
        // `regexp` y est construit sans les drapeaux de correspondance, et
        // `case_insensitive` en est un. Le servir rendrait acceptable une
        // requete qu'un vrai Elasticsearch rejette — c'est le raisonnement de
        // `boost_factor`, applique a un champ plutot qu'a un parametre. Le
        // message est le sien, mot pour mot (mesure contre 8.15.0 : Lucene
        // numerote ce drapeau 256).
        if insensible {
            return Err(EsError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "query_shard_exception",
                "failed to create query: Match flags not yet implemented [256]",
            ));
        }
        return boost(
            motif_dans_la_colonne(champ, &mf, &motif, "regexp")?,
            boost_value.as_ref(),
        );
    }
    let q = RegexQuery::from_pattern(&motif, mf.field).map_err(|e| {
        EsError::illegal_argument(format!(
            "[regexp] : motif [{valeur}] refuse par l'automate : {e}"
        ))
    })?;
    // ES rend un score constant sur une requete multi-termes reecrite.
    boost(
        Box::new(ConstScoreQuery::new(Box::new(q), 1.0)),
        boost_value.as_ref(),
    )
}

/// `fuzzy` : les termes a faible distance d'edition.
///
/// `fuzziness` suit la regle `AUTO` d'ES — 0 sous 3 caracteres, 1 jusqu'a 5,
/// 2 au-dela — ou une distance explicite.
fn fuzzy_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "fuzzy")?;
    let (champ, valeur, boost_value) = valeur_et_boost(
        obj,
        "fuzzy",
        &[
            "fuzziness",
            "transpositions",
            "prefix_length",
            "max_expansions",
            "rewrite",
        ],
    )?;
    let spec = single_key(obj, "fuzzy")?.1;
    let params = spec.as_object();

    if params.is_some_and(|o| o.contains_key("prefix_length")) {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas [prefix_length] dans [fuzzy]",
        ));
    }
    let distance = match params.and_then(|o| o.get("fuzziness")) {
        None => fuzziness_auto(&valeur),
        Some(Value::String(s)) if s.eq_ignore_ascii_case("auto") => fuzziness_auto(&valeur),
        Some(Value::Number(n)) => n.as_u64().unwrap_or(2).min(2) as u8,
        Some(Value::String(s)) => s.parse::<u8>().map(|d| d.min(2)).map_err(|_| {
            EsError::unsupported(format!(
                "ferrite ne supporte que [AUTO] ou une distance entiere pour [fuzziness] (recu \
                 [{s}])"
            ))
        })?,
        Some(_) => return Err(EsError::illegal_argument("[fuzziness] : valeur invalide")),
    };
    // ES compte une transposition comme une seule operation.
    let transpositions = params
        .and_then(|o| o.get("transpositions"))
        .and_then(Value::as_bool)
        .unwrap_or(true);

    // Une distance d'edition se mesure entre deux chaines. Sur un champ
    // numerique ou `date`, ES refuse (« Can only use fuzzy queries on keyword
    // and text fields ») ; ferrite construisait un terme texte sur une colonne
    // qui n'en contient pas et rendait **zero document en 200** — un resultat
    // vide qui se fait passer pour une reponse.
    if let Some(q) = meta_query(
        ctx,
        champ,
        crate::meta::Clause::Fuzzy,
        std::slice::from_ref(&Value::String(valeur.clone())),
    ) {
        return boost(q?, boost_value.as_ref());
    }
    let mf = champ_de_motif(ctx, champ, "fuzzy")?;
    // Non indexe, la distance se mesure contre le dictionnaire de la colonne —
    // avec le **meme** automate que celui que `FuzzyTermQuery` compile, sans
    // quoi les deux chemins ne rendraient pas les memes documents.
    if !mf.indexe {
        if mf.ty.kind() == FieldKind::Text {
            return Err(champ_sans_index(champ));
        }
        let dfa = levenshtein_automata::LevenshteinAutomatonBuilder::new(distance, transpositions)
            .build_dfa(&valeur);
        return boost(
            dans_la_colonne(
                champ,
                mf.ty,
                PredicatColonne::Automate(Automate::Levenshtein(Box::new(dfa))),
            ),
            boost_value.as_ref(),
        );
    }
    let terme = tantivy::Term::from_field_text(mf.field, &valeur);
    let q = FuzzyTermQuery::new(terme, distance, transpositions);
    boost(
        Box::new(ConstScoreQuery::new(Box::new(q), 1.0)),
        boost_value.as_ref(),
    )
}

/// La regle `AUTO` d'Elasticsearch.
fn fuzziness_auto(terme: &str) -> u8 {
    match terme.chars().count() {
        0..=2 => 0,
        3..=5 => 1,
        _ => 2,
    }
}

/// `case_insensitive` : le repliement de casse **ASCII** d'Elasticsearch.
fn lire_insensible(obj: &Map<String, Value>, clause: &str) -> EsResult<bool> {
    let (_, spec) = single_key(obj, clause)?;
    match spec.as_object().and_then(|o| o.get("case_insensitive")) {
        None => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(EsError::illegal_argument(format!(
            "[{clause}] : [case_insensitive] attend un booleen"
        ))),
    }
}

/// `rewrite` choisit la strategie de reecriture d'une requete multi-termes chez
/// Lucene, et avec elle le mode de scoring. ferrite n'en a qu'une (score
/// constant) : l'accepter sans effet changerait les scores en silence.
fn refuser_rewrite(obj: &Map<String, Value>, clause: &str) -> EsResult<()> {
    let (_, spec) = single_key(obj, clause)?;
    if spec.as_object().is_some_and(|o| o.contains_key("rewrite")) {
        return Err(EsError::unsupported(format!(
            "ferrite ne supporte pas [rewrite] dans [{clause}] : il ne reecrit une requete \
             multi-termes que d'une facon, a score constant"
        )));
    }
    Ok(())
}

/// `constant_score` : le filtre decide, le score est fixe.
fn constant_score_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "constant_score")?;
    expect_only(obj, &["filter", "boost"], "constant_score")?;
    let filtre = obj
        .get("filter")
        .ok_or_else(|| EsError::illegal_argument("[constant_score] : [filter] est obligatoire"))?;
    let inner = build_query(filtre, ctx)?;
    let score = match obj.get("boost") {
        None => 1.0,
        Some(v) => v
            .as_f64()
            .ok_or_else(|| EsError::illegal_argument("[boost] : nombre attendu"))?
            as f32,
    };
    Ok(Box::new(ConstScoreQuery::new(inner, score)))
}

/// `dis_max` : le meilleur score l'emporte (voir [`crate::dismax`]).
fn dis_max_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "dis_max")?;
    expect_only(obj, &["queries", "tie_breaker", "boost"], "dis_max")?;
    let queries = obj
        .get("queries")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            EsError::illegal_argument("[dis_max] : [queries] (liste) est obligatoire")
        })?;
    if queries.is_empty() {
        return Err(EsError::illegal_argument(
            "[dis_max.queries] ne peut pas etre vide",
        ));
    }
    let sous: Vec<Box<dyn Query>> = queries
        .iter()
        .map(|q| build_query(q, ctx))
        .collect::<EsResult<_>>()?;
    let tie = match obj.get("tie_breaker") {
        None => 0.0f32,
        Some(v) => v
            .as_f64()
            .ok_or_else(|| EsError::illegal_argument("[tie_breaker] : nombre attendu"))?
            as f32,
    };
    boost(Box::new(DisMaxQuery::new(sous, tie)), obj.get("boost"))
}

// ---------------------------------------------------------------------------
// function_score / boosting : le reglage de la pertinence
// ---------------------------------------------------------------------------

/// Les noms de fonction qu'ES reconnait, servis ou refuses.
///
/// Ils sont listes ensemble parce que le parseur d'ES les traite ensemble : une
/// de ces cles au premier niveau interdit `functions`, et reciproquement. Un nom
/// refuse doit donc etre reconnu **comme un nom de fonction** avant d'etre
/// refuse, sinon le refus se deguise en faute de frappe.
const NOMS_DE_FONCTION: &[&str] = &[
    "weight",
    "field_value_factor",
    "gauss",
    "exp",
    "linear",
    "random_score",
    "script_score",
];

/// `function_score` : la meme requete, mais les documents recents devant.
fn function_score_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "function_score")?;
    let mut sous: Option<Box<dyn Query>> = None;
    let mut mode = ModeDeScore::default();
    let mut combinaison = Combinaison::default();
    // Le defaut d'ES est `Float.MAX_VALUE` : un plafond qui ne plafonne rien.
    let mut plafond = f64::from(f32::MAX);
    let mut minimum: Option<f32> = None;
    let mut valeur_boost: Option<&Value> = None;
    let mut fonctions: Vec<Fonction> = Vec::new();
    // ES refuse de melanger `functions` et une fonction unique, et nomme la cle
    // qu'il a rencontree **en premier**. Le corps est donc lu dans son ordre —
    // `serde_json` le preserve ici (`preserve_order`).
    let mut deja: Option<String> = None;
    let conflit = |deja: &str, cle: &str| {
        EsError::parsing(format!(
            "failed to parse [function_score] query. [you can either define [functions] array or \
             a single function, not both. already found [{deja}], now encountering [{cle}].]"
        ))
    };

    for (cle, valeur) in obj {
        match cle.as_str() {
            "query" => sous = Some(build_query(valeur, ctx)?),
            "score_mode" => {
                let nom = lit_chaine(valeur, "score_mode")?;
                mode = ModeDeScore::lit(&nom).ok_or_else(|| {
                    EsError::illegal_argument(format!(
                        "[function_score.score_mode] : [{nom}] inconnu ; valeurs acceptees : \
                         [multiply, sum, avg, first, max, min]"
                    ))
                })?;
            }
            "boost_mode" => {
                let nom = lit_chaine(valeur, "boost_mode")?;
                combinaison = Combinaison::lit(&nom).ok_or_else(|| {
                    EsError::illegal_argument(format!(
                        "[function_score.boost_mode] : [{nom}] inconnu ; valeurs acceptees : \
                         [multiply, replace, sum, avg, max, min]"
                    ))
                })?;
            }
            "max_boost" => plafond = lit_reel(valeur, "max_boost")?,
            "min_score" => minimum = Some(lit_reel(valeur, "min_score")? as f32),
            "boost" => valeur_boost = Some(valeur),
            "functions" => {
                if let Some(d) = &deja {
                    return Err(conflit(d, "functions"));
                }
                deja = Some("functions] array".to_string());
                let liste = valeur.as_array().ok_or_else(|| {
                    EsError::parsing("[function_score.functions] : une liste est attendue")
                })?;
                for entree in liste {
                    fonctions.push(lit_fonction(entree, ctx)?);
                }
            }
            nom if NOMS_DE_FONCTION.contains(&nom) => {
                match &deja {
                    Some(d) if d == "functions] array" => return Err(conflit(d, nom)),
                    Some(d) => {
                        return Err(EsError::parsing(format!(
                            "failed to parse [function_score] query. already found function \
                             [{d}], now encountering [{nom}]. use [functions] array if you want \
                             to define several functions."
                        )))
                    }
                    None => {}
                }
                deja = Some(nom.to_string());
                let mut entree = Map::new();
                entree.insert(nom.to_string(), valeur.clone());
                fonctions.push(lit_fonction(&Value::Object(entree), ctx)?);
            }
            autre => {
                // Deux phrases, et c'est ES qui les separe : une cle inconnue
                // **avant** toute fonction est une cle inconnue ; apres une
                // fonction, c'est une seconde fonction qu'il croit lire.
                // `boost_factor` tombe dans la premiere, et ce n'est pas un
                // oubli : ES 8.15 le refuse lui aussi (mesure) — le servir
                // serait accepter une requete qu'un vrai Elasticsearch rejette.
                return Err(EsError::parsing(match &deja {
                    Some(d) if d != "functions] array" => format!(
                        "failed to parse [function_score] query. already found function [{d}], \
                         now encountering [{autre}]. use [functions] array if you want to define \
                         several functions."
                    ),
                    Some(d) => conflit(d, autre).reason,
                    None => format!(
                        "failed to parse [function_score] query. field [{autre}] is not supported"
                    ),
                }));
            }
        }
    }

    // Le piege de `score_mode`, et il ne se devine pas : **une seule** fonction
    // sans filtre (ou dont le filtre est un `match_all` litteral) fait
    // construire a ES son autre constructeur, celui qui pose `ScoreMode.FIRST`
    // — le `score_mode` demande est alors purement ignore. Ca ne se voit que
    // sur `avg`, le seul mode qui differe de `first` a une fonction : sur un
    // `weight: 2`, ES rend le score de base x2 alors qu'une moyenne ponderee
    // rendrait x1. Mesure contre ES 8.15, sur les quatre formes
    // (`weight` au premier niveau, `functions` a un element avec et sans
    // filtre, `functions` a deux elements).
    let mode = if fonctions.len() == 1 && fonctions[0].filtre.is_none() {
        ModeDeScore::Premiere
    } else {
        mode
    };
    let sous = sous.unwrap_or_else(|| Box::new(AllQuery) as Box<dyn Query>);
    // Le `boost` entre **dans** la clause au lieu de l'envelopper : voir
    // [`FonctionScore::boost_clause`], c'est le total des hits qui en depend.
    let facteur = match valeur_boost {
        None | Some(Value::Null) => 1.0f32,
        Some(v) => lit_reel(v, "boost")? as f32,
    };
    Ok(Box::new(FonctionScore::new(
        sous,
        fonctions,
        mode,
        combinaison,
        plafond,
        minimum,
        facteur,
        ctx.incidents.clone(),
    )))
}

/// Une entree de `functions[]` — ou la fonction unique du premier niveau,
/// enveloppee pour passer par ici.
fn lit_fonction(entree: &Value, ctx: &QueryCtx) -> EsResult<Fonction> {
    let obj = as_object(entree, "function_score.functions")?;
    let mut filtre = None;
    let mut poids = None;
    let mut calcul: Option<(String, Calcul)> = None;
    for (cle, valeur) in obj {
        match cle.as_str() {
            // Un `filter` qui est un `match_all` **litteral** ne filtre rien —
            // et ES le traite comme absent pour decider s'il prend son
            // constructeur a une fonction (voir `function_score_query`). Le
            // laisser tomber ici reproduit les deux moities a la fois.
            "filter" => {
                filtre = match valeur.as_object().and_then(|o| o.keys().next()) {
                    Some(k) if k == "match_all" && valeur.as_object().unwrap().len() == 1 => {
                        build_query(valeur, ctx)?;
                        None
                    }
                    _ => Some(build_query(valeur, ctx)?),
                };
            }
            "weight" => {
                let p = lit_reel(valeur, "weight")?;
                if p < 0.0 {
                    return Err(EsError::illegal_argument(
                        "[weight] cannot be negative for a filtering function",
                    ));
                }
                poids = Some(p);
            }
            "random_score" | "script_score" => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas [{cle}] dans [function_score] ; fonctions \
                     acceptees : [weight, field_value_factor, gauss, exp, linear]"
                )));
            }
            nom @ ("field_value_factor" | "gauss" | "exp" | "linear") => {
                if let Some((deja, _)) = &calcul {
                    return Err(EsError::parsing(format!(
                        "failed to parse function_score functions. already found [{deja}], now \
                         encountering [{nom}]."
                    )));
                }
                calcul = Some((
                    nom.to_string(),
                    if nom == "field_value_factor" {
                        Calcul::Valeur(lit_valeur_de_champ(valeur, ctx)?)
                    } else {
                        Calcul::Decroit(lit_attenuation(nom, valeur, ctx)?)
                    },
                ));
            }
            autre => {
                return Err(EsError::parsing(format!(
                    "failed to parse [function_score] query. field [{autre}] is not supported"
                )));
            }
        }
    }
    // `weight` seul **est** une fonction ; un `filter` seul n'en est pas une.
    let calcul =
        match calcul {
            Some((_, c)) => c,
            None if poids.is_some() => Calcul::Poids,
            None => return Err(EsError::parsing(
                "failed to parse [function_score] query. an entry in functions list is missing a \
                 function.",
            )),
        };
    Ok(Fonction {
        filtre,
        poids,
        calcul,
    })
}

/// Un refus qu'ES prononce **a l'execution du shard**, et non a la lecture du
/// corps.
///
/// La distinction n'est pas cosmetique : elle decide de ce qu'une recherche
/// multi-index rend. Un `scale` negatif est refuse par le shard, donc les
/// autres index repondent quand meme — c'est ce que fait ES, et c'est ce que le
/// marqueur [`EsError::sur_un_shard`] reproduit ici.
fn refus_de_shard(ty: &str, reason: impl Into<String>) -> EsError {
    EsError::new(axum::http::StatusCode::BAD_REQUEST, ty, reason).sur_un_shard()
}

/// `field_value_factor` : le score est une valeur du document.
fn lit_valeur_de_champ(body: &Value, ctx: &QueryCtx) -> EsResult<ValeurDeChamp> {
    let obj = obj_de_fonction(body, "field_value_factor")?;
    expect_only(
        obj,
        &["field", "factor", "modifier", "missing"],
        "field_value_factor",
    )?;
    let champ = obj
        .get("field")
        .and_then(Value::as_str)
        .ok_or_else(|| EsError::parsing("[field_value_factor] required field 'field' missing"))?;
    let modificateur = match obj.get("modifier") {
        None | Some(Value::Null) => Modificateur::default(),
        Some(v) => {
            let nom = lit_chaine(v, "modifier")?;
            Modificateur::lit(&nom).ok_or_else(|| {
                EsError::illegal_argument(format!(
                    "[field_value_factor.modifier] : [{nom}] inconnu ; valeurs acceptees : \
                     [none, log, log1p, log2p, ln, ln1p, ln2p, square, sqrt, reciprocal]"
                ))
            })?
        }
    };
    Ok(ValeurDeChamp {
        champ: champ.to_string(),
        genre: genre_numerique(champ, "field_value_factor", ctx)?,
        facteur: match obj.get("factor") {
            None | Some(Value::Null) => 1.0,
            Some(v) => lit_reel(v, "factor")?,
        },
        modificateur,
        manquant: match obj.get("missing") {
            None | Some(Value::Null) => None,
            Some(v) => Some(lit_reel(v, "missing")?),
        },
    })
}

/// `gauss` / `exp` / `linear` : le score decroit avec la distance a `origin`.
fn lit_attenuation(nom: &str, body: &Value, ctx: &QueryCtx) -> EsResult<Attenuation> {
    let fonction = Decroissance::lit(nom).expect("nom deja filtre");
    let obj = obj_de_fonction(body, nom)?;
    let mut champ: Option<(&str, &Value)> = None;
    for (cle, valeur) in obj {
        match cle.as_str() {
            // `multi_value_mode` est refuse en le nommant : son defaut (`min`,
            // pose sur la **distance** et non sur la valeur) est celui que
            // ferrite applique, et servir les trois autres sans les mesurer
            // rendrait des scores silencieusement differents.
            "multi_value_mode" => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas [multi_value_mode] dans [{nom}] ; seul le defaut \
                     d'Elasticsearch ([min], applique a la distance) est servi"
                )))
            }
            // ES accepte plusieurs champs dans une meme decroissance et n'en
            // applique **qu'un**, sans dire lequel (mesure). Le reproduire
            // demanderait de deviner lequel ; ferrite refuse en le nommant.
            _ if champ.is_some() => {
                return Err(EsError::unsupported(format!(
                    "ferrite ne supporte pas plusieurs champs dans un [{nom}] (deja [{}], puis \
                     [{cle}]) ; Elasticsearch en applique un seul sans dire lequel",
                    champ.expect("verifie juste au-dessus").0
                )));
            }
            _ => champ = Some((cle.as_str(), valeur)),
        }
    }
    let Some((champ, spec)) = champ else {
        return Err(EsError::parsing(
            "malformed score function score parameters.",
        ));
    };
    let spec = as_object(spec, nom)?;
    expect_only(spec, &["origin", "scale", "offset", "decay"], nom)?;
    let decay = match spec.get("decay") {
        None | Some(Value::Null) => 0.5,
        Some(v) => lit_reel(v, "decay")?,
    };
    if !(decay > 0.0 && decay < 1.0) {
        // NaN compris
        return Err(refus_de_shard(
            "query_shard_exception",
            "failed to create query: function_score : decay must be in the range [0..1].",
        ));
    }
    let genre = genre_numerique(champ, nom, ctx)?;
    // Une decroissance sur un champ que le mapping ne connait pas est refusee
    // par ES (mesure) — la ou son `field_value_factor` l'accepte et sert son
    // `missing`. Les deux fonctions ne lisent pas le mapping au meme moment.
    //
    // Le refus est marque **echec de shard** bien qu'ES lui donne le type
    // `parsing_exception` : c'est un verdict de mapping, pas de forme. Sans
    // cette marque, `_validate/query` le prendrait pour une erreur de
    // coordinateur et rendrait `valid: false` sur une requete qu'ES declare
    // valide — le piege deja paye sur `nested`, retrouve par le fuzzer.
    if genre.is_none() {
        return Err(refus_de_shard(
            "parsing_exception",
            format!("unknown field [{champ}]"),
        ));
    }
    // Sur une date, `origin` est une **expression** (`now`, `2026-03-15||+1M`)
    // et `scale` / `offset` sont des durees ; ailleurs, ce sont des nombres.
    // ES a deux parseurs, et il n'accepte pas l'un a la place de l'autre.
    let sur_date = genre == Some(crate::fonction_score::GenreNumerique::Date);
    let (origine, echelle, offset) = if sur_date {
        let origine = match spec.get("origin") {
            // Le defaut d'ES sur une date est `now` — et c'est le seul type ou
            // `origin` est facultatif.
            None | Some(Value::Null) => ctx.maintenant as f64,
            Some(v) => datemath::borne_dans(
                v,
                ctx.fields.format_ou_defaut(champ),
                ctx.maintenant,
                Arrondi::Bas,
                &crate::fuseau::Fuseau::utc(),
            )? as f64,
        };
        (
            origine,
            lit_duree(spec.get("scale"), "scale", nom)?,
            match spec.get("offset") {
                None | Some(Value::Null) => 0.0,
                v => lit_duree(v, "offset", nom)?,
            },
        )
    } else {
        let Some(o) = spec.get("origin").filter(|v| !v.is_null()) else {
            return Err(refus_de_shard(
                "parse_exception",
                "both [scale] and [origin] must be set for numeric fields.",
            ));
        };
        let Some(s) = spec.get("scale").filter(|v| !v.is_null()) else {
            return Err(refus_de_shard(
                "parse_exception",
                "both [scale] and [origin] must be set for numeric fields.",
            ));
        };
        (
            lit_reel(o, "origin")?,
            lit_reel(s, "scale")?,
            match spec.get("offset") {
                None | Some(Value::Null) => 0.0,
                Some(v) => lit_reel(v, "offset")?,
            },
        )
    };
    // `NaN` compris : une echelle illisible n'est pas une echelle valide.
    if !matches!(echelle.partial_cmp(&0.0), Some(std::cmp::Ordering::Greater)) {
        return Err(refus_de_shard(
            "query_shard_exception",
            "failed to create query: function_score : scale must be > 0.0.",
        ));
    }
    if offset < 0.0 {
        return Err(refus_de_shard(
            "query_shard_exception",
            "failed to create query: function_score : offset must be > 0.0",
        ));
    }
    Ok(Attenuation {
        champ: champ.to_string(),
        genre,
        fonction,
        origine,
        // `processScale` une fois pour toutes : la formule ne le refait pas par
        // document.
        echelle: fonction.echelle(echelle, decay),
        offset,
    })
}

/// Le corps d'une fonction, qui doit etre un objet.
///
/// ES range la forme courte (`"field_value_factor": "vues"`) dans « field
/// [field_value_factor] is not supported » plutot que dans une erreur de type :
/// c'est son parseur de fonctions qui n'a rien a lire.
fn obj_de_fonction<'a>(body: &'a Value, nom: &str) -> EsResult<&'a Map<String, Value>> {
    body.as_object().ok_or_else(|| {
        EsError::parsing(format!(
            "failed to parse [function_score] query. field [{nom}] is not supported"
        ))
    })
}

/// De quelle colonne une fonction lit ses valeurs.
///
/// Un champ que le mapping ne connait pas n'a pas de colonne, et ce n'est pas
/// une erreur : ES sert le `field_value_factor` d'un champ inexistant (avec son
/// `missing`) et rend une distance nulle sur une decroissance. Un champ textuel,
/// lui, est refuse — ES le refuse aussi, par un message qui cite une classe
/// Java.
fn genre_numerique(
    champ: &str,
    clause: &str,
    ctx: &QueryCtx,
) -> EsResult<Option<crate::fonction_score::GenreNumerique>> {
    use crate::fonction_score::GenreNumerique as G;
    let f = match ctx.field(champ, clause) {
        Ok(f) => f,
        Err(e) if ctx.champ_inconnu_tolere(&e) => return Ok(None),
        Err(e) => return Err(e),
    };
    Ok(Some(match f.ty.kind() {
        FieldKind::I64 => G::I64,
        FieldKind::F64 => G::F64,
        FieldKind::Date => G::Date,
        FieldKind::Bool => G::Bool,
        FieldKind::Text | FieldKind::Keyword => {
            return Err(refus_de_shard(
                "query_shard_exception",
                format!(
                    "failed to create query: le champ [{champ}] n'est pas numerique (clause \
                     [{clause}]) ; [function_score] lit une colonne de nombres, de dates ou de \
                     booleens — Elasticsearch le refuse aussi"
                ),
            ))
        }
    }))
}

/// Une duree d'ES (`10d`, `500ms`, `0`) en millisecondes.
fn lit_duree(v: Option<&Value>, cle: &str, clause: &str) -> EsResult<f64> {
    let refus = |valeur: &str| {
        refus_de_shard(
            "query_shard_exception",
            format!(
                "failed to create query: failed to parse setting [DecayFunctionParser.{cle}] \
                 with value [{valeur}] as a time value: unit is missing or unrecognized"
            ),
        )
    };
    match v {
        None | Some(Value::Null) => Err(EsError::parsing(format!(
            "[{clause}] : [scale] est obligatoire"
        ))),
        Some(Value::String(s)) => {
            if s.trim() == "0" {
                return Ok(0.0);
            }
            crate::calendrier::lit_fixe(s)
                .map(|ms| ms as f64)
                .ok_or_else(|| refus(s))
        }
        Some(autre) => Err(refus(&autre.to_string())),
    }
}

/// Un nombre, ecrit en nombre ou en chaine — ES lit les deux.
fn lit_reel(v: &Value, cle: &str) -> EsResult<f64> {
    match v {
        Value::Number(n) => n
            .as_f64()
            .ok_or_else(|| EsError::illegal_argument(format!("[{cle}] : nombre attendu"))),
        Value::String(s) => s.parse().map_err(|_| {
            EsError::illegal_argument(format!("[{cle}] : nombre attendu, recu [{s}]"))
        }),
        autre => Err(EsError::illegal_argument(format!(
            "[{cle}] : nombre attendu, recu {autre}"
        ))),
    }
}

fn lit_chaine(v: &Value, cle: &str) -> EsResult<String> {
    v.as_str()
        .map(str::to_string)
        .ok_or_else(|| EsError::illegal_argument(format!("[{cle}] : une chaine est attendue")))
}

/// `boosting` : la demotion sans exclusion (voir [`crate::fonction_score`]).
fn boosting_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "boosting")?;
    for cle in obj.keys() {
        if !["positive", "negative", "negative_boost", "boost"].contains(&cle.as_str()) {
            return Err(EsError::parsing(format!(
                "[boosting] query does not support [{cle}]"
            )));
        }
    }
    let positive = obj
        .get("positive")
        .filter(|v| !v.is_null())
        .ok_or_else(|| EsError::parsing("[boosting] query requires 'positive' query to be set'"))?;
    let negative = obj
        .get("negative")
        .filter(|v| !v.is_null())
        .ok_or_else(|| EsError::parsing("[boosting] query requires 'negative' query to be set'"))?;
    // `negative_boost` est **obligatoire** et doit etre positif : ES ne lui
    // donne pas de defaut, et sa phrase de refus est celle-ci.
    let poids = match obj.get("negative_boost") {
        Some(v) if !v.is_null() => lit_reel(v, "negative_boost")?,
        _ => -1.0,
    };
    if poids < 0.0 {
        return Err(EsError::parsing(
            "[boosting] query requires 'negative_boost' to be set to be a positive value'",
        ));
    }
    let q: Box<dyn Query> = Box::new(Retrograde::new(
        build_query(positive, ctx)?,
        build_query(negative, ctx)?,
        poids as f32,
    ));
    boost(q, obj.get("boost"))
}

fn boost(query: Box<dyn Query>, value: Option<&Value>) -> EsResult<Box<dyn Query>> {
    match value {
        None | Some(Value::Null) => Ok(query),
        Some(v) => {
            let b = v
                .as_f64()
                .ok_or_else(|| EsError::illegal_argument("[boost] : nombre attendu"))?;
            Ok(Box::new(BoostQuery::new(query, b as f32)))
        }
    }
}

fn as_object<'a>(v: &'a Value, clause: &str) -> EsResult<&'a Map<String, Value>> {
    v.as_object()
        .ok_or_else(|| EsError::parsing(format!("[{clause}] doit etre un objet JSON")))
}

fn single_key<'a>(obj: &'a Map<String, Value>, clause: &str) -> EsResult<(&'a str, &'a Value)> {
    let mut it = obj.iter();
    let Some((k, v)) = it.next() else {
        return Err(EsError::parsing(format!(
            "[{clause}] : objet vide, une clause est attendue"
        )));
    };
    if it.next().is_some() {
        return Err(EsError::parsing(format!(
            "[{clause}] n'accepte qu'une seule cle, {} fournies",
            obj.len()
        )));
    }
    Ok((k.as_str(), v))
}

fn expect_only(obj: &Map<String, Value>, allowed: &[&str], clause: &str) -> EsResult<()> {
    for key in obj.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [{key}] dans [{clause}] ; parametres acceptes : {allowed:?}"
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// nested
// ---------------------------------------------------------------------------

/// `{"nested": {"path": "lignes", "query": {...}}}`.
///
/// La requete interne sert deux fois : telle quelle comme **pre-filtre** (elle
/// rend un sur-ensemble exact, avec les postings et le score de tantivy), et
/// traduite en [`Clause`] pour la **verification** element par element. Voir
/// [`crate::nested`] pour le pourquoi de ce double emploi.
fn nested_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "nested")?;
    expect_only(
        obj,
        &[
            "path",
            "query",
            "score_mode",
            "boost",
            "inner_hits",
            "ignore_unmapped",
        ],
        "nested",
    )?;
    for refuse in ["inner_hits", "ignore_unmapped"] {
        if obj.contains_key(refuse) {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [{refuse}] dans [nested]"
            )));
        }
    }
    // Le score d'une clause `nested` est celui du pre-filtre, calcule a plat :
    // ferrite n'a pas de document par element, donc pas de score par element.
    // Le dire plutot que de rendre un classement qui n'est pas celui demande.
    let mut sans_score = false;
    if let Some(mode) = obj.get("score_mode").and_then(Value::as_str) {
        if mode != "none" && mode != "avg" {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [score_mode: {mode}] dans [nested] ; le score est celui \
                 de la requete interne evaluee a plat (voir docs/compat.md)"
            )));
        }
        // `score_mode: none` **est** un score, et il vaut zero : ES y construit
        // un filtre, pas une jointure notee. ferrite rendait le score du
        // pre-filtre (1.0 sur un `exists`), et ca ne se voyait nulle part —
        // un facteur constant ne change pas un ensemble de documents. Sauf
        // sous un `min_score`, qui en fait un seuil : `min_score: 1` y gardait
        // 15 documents la ou ES n'en rend aucun, en 200. Trouve par le fuzzer
        // (graine 9410019) le jour ou `min_score` lui a donne de quoi le voir.
        sans_score = mode == "none";
    }

    let path = obj
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| EsError::parsing("[nested] : cle [path] manquante"))?;
    // Sans index vise, aucun mapping ne peut dire si ce chemin est `nested` :
    // le verdict est suspendu pour que la requete interne, elle, soit lue.
    if !ctx.aucun_index_vise && !ctx.fields.nested.contains(path) {
        return Err(EsError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "query_shard_exception",
            format!("[nested] : [{path}] n'est pas un champ de type [nested] dans ce mapping"),
        ));
    }
    let interne = obj
        .get("query")
        .ok_or_else(|| EsError::parsing("[nested] : cle [query] manquante"))?;

    ctx.nested_ouvert.borrow_mut().push(path.to_string());
    let construit = (|| {
        // Le pre-filtre doit etre un **sur-ensemble** des documents attendus.
        // Une negation ne l'est pas : `must_not: ref=vis` ecarterait a plat un
        // document dont une *autre* ligne satisfait la clause. On la retire
        // donc du pre-filtre — la verification par element, elle, la garde.
        let prefiltre = build_query(&sans_negations(interne), ctx)?;
        let clause = clause_nested(interne, ctx, path)?;
        EsResult::Ok((prefiltre, clause))
    })();
    ctx.nested_ouvert.borrow_mut().pop();
    let (prefiltre, clause) = construit?;
    let mut q: Box<dyn Query> = Box::new(NestedQuery::new(path.to_string(), prefiltre, clause)?);
    if sans_score {
        q = Box::new(ConstScoreQuery::new(q, 0.0));
    }
    boost(q, obj.get("boost"))
}

/// Traduit la requete interne d'un `nested` en conditions verifiables sur les
/// colonnes. Tout ce qui ne s'y reduit pas est refuse explicitement.
fn clause_nested(v: &Value, ctx: &QueryCtx, racine: &str) -> EsResult<Clause> {
    let obj = as_object(v, "nested.query")?;
    let (name, body) = single_key(obj, "nested.query")?;
    match name {
        "match_all" => Ok(Clause::Tous),
        "match_none" => Ok(Clause::Aucun),
        "term" | "match" => {
            let o = as_object(body, name)?;
            let (champ, spec) = single_key(o, name)?;
            let valeur = match spec {
                Value::Object(sub) => sub
                    .get("value")
                    .or_else(|| sub.get("query"))
                    .cloned()
                    .ok_or_else(|| EsError::parsing(format!("[{name}] : valeur manquante")))?,
                v => v.clone(),
            };
            let mf = champ_nested(ctx, champ, racine, name)?;
            // Sur une date, `term` designe une periode, pas un instant : c'est
            // un intervalle, comme a la racine (voir [`periode_date`]).
            if mf.ty.kind() == FieldKind::Date {
                let (bas, haut) = periode_nested(ctx, champ, &valeur)?;
                return Ok(Clause::Champ {
                    chemin: champ.to_string(),
                    champ: mf,
                    predicat: Predicat::Intervalle(bas, haut),
                });
            }
            Ok(Clause::Champ {
                chemin: champ.to_string(),
                champ: mf,
                predicat: Predicat::Parmi(vec![valeur_de(ctx, champ, mf, &valeur)?]),
            })
        }
        "terms" => {
            let o = as_object(body, "terms")?;
            let (champ, spec) = single_key(o, "terms")?;
            let liste = spec
                .as_array()
                .ok_or_else(|| EsError::parsing("[terms] attend un tableau"))?;
            let mf = champ_nested(ctx, champ, racine, "terms")?;
            // Chaque date est une periode : l'ensemble devient une union
            // d'intervalles, pas un ensemble de valeurs.
            if mf.ty.kind() == FieldKind::Date {
                let clauses = liste
                    .iter()
                    .map(|v| {
                        let (bas, haut) = periode_nested(ctx, champ, v)?;
                        Ok(Clause::Champ {
                            chemin: champ.to_string(),
                            champ: mf,
                            predicat: Predicat::Intervalle(bas, haut),
                        })
                    })
                    .collect::<EsResult<Vec<_>>>()?;
                return Ok(Clause::Ou(clauses, 1));
            }
            let vals = liste
                .iter()
                .map(|v| valeur_de(ctx, champ, mf, v))
                .collect::<EsResult<Vec<_>>>()?;
            Ok(Clause::Champ {
                chemin: champ.to_string(),
                champ: mf,
                predicat: Predicat::Parmi(vals),
            })
        }
        "range" => {
            let o = as_object(body, "range")?;
            let (champ, spec) = single_key(o, "range")?;
            let spec = as_object(spec, "range")?;
            let mf = champ_nested(ctx, champ, racine, "range")?;
            let borne = |cle: &str| -> EsResult<Option<Valeur>> {
                match spec.get(cle) {
                    None | Some(Value::Null) => Ok(None),
                    Some(v) if mf.ty.kind() == FieldKind::Date => {
                        let sens = if cle == "gte" || cle == "lt" {
                            Arrondi::Bas
                        } else {
                            Arrondi::Haut
                        };
                        let format = ctx.fields.format_ou_defaut(champ);
                        Ok(Some(Valeur::I64(datemath::borne(
                            v,
                            format,
                            ctx.maintenant,
                            sens,
                        )?)))
                    }
                    Some(v) => Ok(Some(valeur_de(ctx, champ, mf, v)?)),
                }
            };
            let bas = match (borne("gte")?, borne("gt")?) {
                (Some(v), None) => Bound::Included(v),
                (None, Some(v)) => Bound::Excluded(v),
                (None, None) => Bound::Unbounded,
                (Some(_), Some(_)) => {
                    return Err(EsError::illegal_argument(
                        "[range] : [gte] et [gt] sont mutuellement exclusifs",
                    ))
                }
            };
            let haut = match (borne("lte")?, borne("lt")?) {
                (Some(v), None) => Bound::Included(v),
                (None, Some(v)) => Bound::Excluded(v),
                (None, None) => Bound::Unbounded,
                (Some(_), Some(_)) => {
                    return Err(EsError::illegal_argument(
                        "[range] : [lte] et [lt] sont mutuellement exclusifs",
                    ))
                }
            };
            Ok(Clause::Champ {
                chemin: champ.to_string(),
                champ: mf,
                predicat: Predicat::Intervalle(bas, haut),
            })
        }
        "exists" => {
            let o = as_object(body, "exists")?;
            let champ = o
                .get("field")
                .and_then(Value::as_str)
                .ok_or_else(|| EsError::parsing("[exists] : cle [field] manquante"))?;
            let mf = champ_nested(ctx, champ, racine, "exists")?;
            Ok(Clause::Champ {
                chemin: champ.to_string(),
                champ: mf,
                predicat: Predicat::Existe,
            })
        }
        "prefix" => {
            let o = as_object(body, "prefix")?;
            let (champ, spec) = single_key(o, "prefix")?;
            let valeur = match spec {
                Value::Object(sub) => sub.get("value").cloned().unwrap_or(Value::Null),
                v => v.clone(),
            };
            let prefixe = valeur
                .as_str()
                .ok_or_else(|| EsError::parsing("[prefix] attend une chaine"))?;
            let mf = champ_nested(ctx, champ, racine, "prefix")?;
            // La meme regle qu'a la racine : un prefixe n'a de sens que sur une
            // chaine. La verification manquait ici, et un `prefix` sur un champ
            // `date` sous un `nested` rendait 200 la ou ES refuse — le genre de
            // 200 qui compte des documents au hasard.
            if !matches!(mf.ty.kind(), FieldKind::Keyword | FieldKind::Text) {
                return Err(EsError::illegal_argument(format!(
                    "[prefix] ne s'applique qu'a un champ [text] ou [keyword] ; [{champ}] est de \
                     type [{}]",
                    mf.ty.name()
                )));
            }
            Ok(Clause::Champ {
                chemin: champ.to_string(),
                champ: mf,
                predicat: Predicat::Prefixe(prefixe.to_string()),
            })
        }
        "bool" => {
            let o = as_object(body, "bool")?;
            expect_only(
                o,
                &[
                    "must",
                    "filter",
                    "should",
                    "must_not",
                    MUST_NOT_CAMEL,
                    "minimum_should_match",
                    "boost",
                ],
                "nested.bool",
            )?;
            if o.contains_key("must_not") && o.contains_key(MUST_NOT_CAMEL) {
                return Err(EsError::parsing(
                    "[bool] : [must_not] et [mustNot] sont deux ecritures du meme parametre",
                ));
            }
            let liste = |cle: &str| -> EsResult<Vec<Clause>> {
                match o.get(cle) {
                    None => Ok(Vec::new()),
                    Some(Value::Array(a)) => a
                        .iter()
                        .map(|c| clause_nested(c, ctx, racine))
                        .collect::<EsResult<Vec<_>>>(),
                    Some(v) => Ok(vec![clause_nested(v, ctx, racine)?]),
                }
            };
            let mut et = liste("must")?;
            et.extend(liste("filter")?);
            let should = liste("should")?;
            let mut must_not = liste("must_not")?;
            must_not.extend(liste(MUST_NOT_CAMEL)?);
            if !should.is_empty() {
                // Meme regle que chez ES : `should` seul exige un match, `should`
                // accompagne d'un `must` est facultatif sauf minimum explicite.
                // Un `must_not` **ne rend pas** le `should` facultatif : seule
                // une clause obligatoire le fait. La version qui le croyait
                // rendait des documents qu'ES ne rend pas, en silence — un
                // element sans aucun `should` suffisait des lors qu'un *autre*
                // element du meme document passait le pre-filtre (mesure :
                // `tests/compat/sonde_msm.py`, document `ny`).
                //
                // Et ce n'est pas seulement la **valeur par defaut** qui est en
                // jeu : un `minimum_should_match` explicite qui *retombe* a
                // zero — `"50%"` d'une seule clause, la troncature vers zero
                // d'ES le rend nul — ne rend pas non plus le `should`
                // facultatif. Lucene exige au moins une clause positive quand
                // il n'y a aucune clause obligatoire, quel que soit le minimum
                // demande. ferrite jetait alors le `should` entier : un
                // document dont un element satisfaisait seulement le
                // `must_not` remontait, la ou ES n'en rend aucun (trouve par
                // une plage de controle du fuzzer, graine 4242047 ; mesure
                // reduite dans `sonde_msm.py`).
                let defaut = usize::from(et.is_empty());
                let minimum =
                    crate::msm::resoudre(o.get("minimum_should_match"), should.len(), defaut)?
                        .max(defaut);
                if minimum > 0 {
                    et.push(Clause::Ou(should, minimum));
                }
            }
            for c in must_not {
                et.push(Clause::Non(Box::new(c)));
            }
            Ok(if et.is_empty() {
                Clause::Tous
            } else {
                Clause::Et(et)
            })
        }
        autre => Err(EsError::unsupported(format!(
            "ferrite ne supporte pas [{autre}] dans une clause [nested] ; clauses verifiables \
             element par element : match_all, match_none, term, terms, match, range, exists, \
             prefix, bool"
        ))),
    }
}

/// Le champ vise par une clause interne : il doit vivre sous la racine
/// `nested`, sinon la correlation par element n'a pas de sens.
fn champ_nested(ctx: &QueryCtx, champ: &str, racine: &str, clause: &str) -> EsResult<MappedField> {
    if !mapping::est_sous_chemin(champ, racine) {
        return Err(EsError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "query_shard_exception",
            format!(
                "[nested] sur [{racine}] : le champ [{champ}] n'est pas sous ce chemin (clause \
                 [{clause}])"
            ),
        ));
    }
    let mf = ctx.field(champ, clause)?;
    if mf.ty.kind() == FieldKind::Text {
        return Err(EsError::unsupported(format!(
            "ferrite ne verifie pas un champ [text] element par element (champ [{champ}] dans un \
             [nested]) : les colonnes portent la valeur, pas les termes analyses. Interroge son \
             multi-field [keyword], ou sors la clause du [nested]"
        )));
    }
    if mf.elem.is_none() {
        return Err(EsError::internal(format!(
            "[{champ}] n'a pas de colonne d'element : index construit avant le support [nested] ?"
        )));
    }
    Ok(mf)
}

/// La periode qu'une date designe, en bornes de [`Valeur`] (voir
/// [`periode_date`], sa jumelle a la racine).
fn periode_nested(
    ctx: &QueryCtx,
    champ: &str,
    v: &Value,
) -> EsResult<(Bound<Valeur>, Bound<Valeur>)> {
    let format = ctx.fields.format_ou_defaut(champ);
    let bas = datemath::borne(v, format, ctx.maintenant, Arrondi::Bas)?;
    let haut = datemath::borne(v, format, ctx.maintenant, Arrondi::Haut)?;
    Ok((
        Bound::Included(Valeur::I64(bas)),
        Bound::Included(Valeur::I64(haut)),
    ))
}

/// Convertit une valeur JSON au type du champ, puis en [`Valeur`] comparable.
fn valeur_de(ctx: &QueryCtx, champ: &str, mf: MappedField, v: &Value) -> EsResult<Valeur> {
    Ok(
        match mapping::coerce_avec(champ, mf.ty, v, ctx.fields.format_de(champ))? {
            TypedValue::Str(s) => Valeur::Str(s),
            TypedValue::I64(n) => Valeur::I64(n),
            TypedValue::F64(n) => Valeur::F64(n),
            TypedValue::Bool(b) => Valeur::Bool(b),
            TypedValue::Date(ms) => Valeur::I64(ms),
        },
    )
}

/// La requete privee de ses `must_not`, a tous les niveaux.
///
/// Le resultat est **plus large** que l'original : c'est exactement ce qu'un
/// pre-filtre doit etre.
fn sans_negations(v: &Value) -> Value {
    match v {
        Value::Object(o) => Value::Object(
            o.iter()
                // Les deux ecritures : oublier la seconde rendrait le
                // pre-filtre **plus etroit** que la requete, et un document
                // dont une autre ligne satisfait la clause disparaitrait.
                .filter(|(k, _)| k.as_str() != "must_not" && k.as_str() != MUST_NOT_CAMEL)
                .map(|(k, sous)| (k.clone(), sans_negations(sous)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(sans_negations).collect()),
        autre => autre.clone(),
    }
}

// ---------------------------------------------------------------------------
// join : has_child / has_parent / parent_id
// ---------------------------------------------------------------------------

/// De quel cote de la relation la requete interne s'applique.
#[derive(Clone, Copy, PartialEq)]
enum Sens {
    /// `has_child` : la requete porte sur les enfants, on rend les parents.
    VersLeParent,
    /// `has_parent` : la requete porte sur les parents, on rend les enfants.
    VersLEnfant,
}

/// `has_child` / `has_parent`, en deux passes.
///
/// 1. la requete interne est executee, restreinte a la relation visee ;
/// 2. les identifiants qui en sortent (celui du parent pour un enfant, le sien
///    pour un parent) deviennent une recherche sur `_id` ou sur la colonne du
///    parent.
///
/// Exact, et borne par le nombre d'identifiants distincts. Elasticsearch ne
/// peut pas se le permettre — distribue, il lui faut des *global ordinals* —
/// mais mono-shard, parent et enfant sont forcement au meme endroit.
fn join_query(body: &Value, ctx: &QueryCtx, sens: Sens) -> EsResult<Box<dyn Query>> {
    let nom_clause = if sens == Sens::VersLeParent {
        "has_child"
    } else {
        "has_parent"
    };
    let obj = as_object(body, nom_clause)?;
    let cle_type = if sens == Sens::VersLeParent {
        "type"
    } else {
        "parent_type"
    };
    expect_only(
        obj,
        &[
            cle_type,
            "query",
            "score_mode",
            "boost",
            "inner_hits",
            "ignore_unmapped",
            "min_children",
            "max_children",
        ],
        nom_clause,
    )?;
    for refuse in [
        "inner_hits",
        "ignore_unmapped",
        "min_children",
        "max_children",
    ] {
        if obj.contains_key(refuse) {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [{refuse}] dans [{nom_clause}]"
            )));
        }
    }
    if let Some(mode) = obj.get("score_mode").and_then(Value::as_str) {
        if mode != "none" {
            return Err(EsError::unsupported(format!(
                "ferrite ne supporte pas [score_mode: {mode}] dans [{nom_clause}] : la jointure \
                 rend un score constant (voir docs/compat.md)"
            )));
        }
    }

    // Sans index vise, il n'y a pas de champ [join] ou chercher la relation :
    // reste a lire la requete interne, qui est tout ce qui peut encore etre
    // faux dans ce corps.
    if ctx.aucun_index_vise {
        let interne = obj
            .get("query")
            .ok_or_else(|| EsError::parsing(format!("[{nom_clause}] : cle [query] manquante")))?;
        build_query(interne, ctx)?;
        return Ok(Box::new(EmptyQuery));
    }

    let (join, f_nom, f_parent) = infos_join(ctx, nom_clause)?;
    let relation = obj
        .get(cle_type)
        .and_then(Value::as_str)
        .ok_or_else(|| EsError::parsing(format!("[{nom_clause}] : cle [{cle_type}] manquante")))?;
    let attendu_enfant = sens == Sens::VersLeParent;
    let est_enfant = join.parent_de(relation).is_some();
    if !join.connait(relation) || est_enfant != attendu_enfant {
        return Err(EsError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "query_shard_exception",
            format!(
                "[{nom_clause}] : [{relation}] n'est pas {} declare dans [{}] ; relations : {:?}",
                if attendu_enfant {
                    "un enfant"
                } else {
                    "un parent"
                },
                join.champ,
                join.noms()
            ),
        ));
    }
    let interne = obj
        .get("query")
        .ok_or_else(|| EsError::parsing(format!("[{nom_clause}] : cle [query] manquante")))?;

    // Passe 1 : les documents du cote interroge, restreints a la relation.
    let cible = BooleanQuery::new(vec![
        (Occur::Must, build_query(interne, ctx)?),
        (
            Occur::Must,
            Box::new(TermQuery::new(
                Term::from_field_text(f_nom, relation),
                IndexRecordOption::Basic,
            )),
        ),
    ]);

    // Passe 2 : de ces documents aux identifiants qui les relient.
    let (lu, vise) = if sens == Sens::VersLeParent {
        (f_parent, ctx.fields.id) // l'enfant porte l'id du parent
    } else {
        (ctx.fields.id, f_parent) // le parent porte le sien
    };
    let ids = collecte_termes(ctx, &cible, lu)?;
    if ids.is_empty() {
        return Ok(Box::new(EmptyQuery));
    }
    let termes: Vec<Term> = ids
        .iter()
        .map(|id| Term::from_field_text(vise, id))
        .collect();
    let inner: Box<dyn Query> = Box::new(ConstScoreQuery::new(
        Box::new(TermSetQuery::new(termes)),
        1.0,
    ));
    boost(inner, obj.get("boost"))
}

/// `{"parent_id": {"type": "ligne", "id": "1"}}` : les enfants d'un parent.
fn parent_id_query(body: &Value, ctx: &QueryCtx) -> EsResult<Box<dyn Query>> {
    let obj = as_object(body, "parent_id")?;
    expect_only(
        obj,
        &["type", "id", "boost", "ignore_unmapped"],
        "parent_id",
    )?;
    if obj.contains_key("ignore_unmapped") {
        return Err(EsError::unsupported(
            "ferrite ne supporte pas [ignore_unmapped] dans [parent_id]",
        ));
    }
    let relation = obj
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| EsError::parsing("[parent_id] : cle [type] manquante"))?;
    let id = match obj.get("id") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return Err(EsError::parsing("[parent_id] : cle [id] manquante")),
    };
    // Sans index vise, il n'y a pas de champ [join] ou verifier la relation ;
    // les deux cles du corps, elles, viennent d'etre lues.
    if ctx.aucun_index_vise {
        return Ok(Box::new(EmptyQuery));
    }
    let (join, f_nom, f_parent) = infos_join(ctx, "parent_id")?;
    if join.parent_de(relation).is_none() {
        return Err(EsError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "query_shard_exception",
            format!(
                "[parent_id] : [{relation}] n'est pas un enfant declare dans [{}]",
                join.champ
            ),
        ));
    }
    let inner: Box<dyn Query> = Box::new(ConstScoreQuery::new(
        Box::new(BooleanQuery::new(vec![
            (
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(f_nom, relation),
                    IndexRecordOption::Basic,
                )) as Box<dyn Query>,
            ),
            (
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(f_parent, &id),
                    IndexRecordOption::Basic,
                )),
            ),
        ])),
        1.0,
    ));
    boost(inner, obj.get("boost"))
}

fn infos_join<'a>(
    ctx: &'a QueryCtx,
    clause: &str,
) -> EsResult<(
    &'a mapping::Join,
    tantivy::schema::Field,
    tantivy::schema::Field,
)> {
    match (
        ctx.fields.join.as_ref(),
        ctx.fields.join_name,
        ctx.fields.join_parent,
    ) {
        (Some(j), Some(n), Some(p)) => Ok((j, n, p)),
        _ => Err(EsError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "query_shard_exception",
            format!("[{clause}] : cet index n'a pas de champ [join]"),
        )),
    }
}

/// Les valeurs distinctes d'une colonne `keyword`, pour les documents qui
/// correspondent a `q`.
fn collecte_termes(
    ctx: &QueryCtx,
    q: &dyn Query,
    champ: tantivy::schema::Field,
) -> EsResult<Vec<String>> {
    use std::collections::BTreeSet;
    let nom = ctx.searcher.schema().get_field_name(champ).to_string();
    let mut out = BTreeSet::new();
    let poids = q.weight(tantivy::query::EnableScoring::disabled_from_searcher(
        ctx.searcher,
    ))?;
    for reader in ctx.searcher.segment_readers() {
        let Ok(Some(col)) = reader.fast_fields().str(&nom) else {
            continue;
        };
        let mut docset = poids.scorer(reader, 1.0)?;
        let mut buf = String::new();
        let mut doc = docset.doc();
        while doc != tantivy::TERMINATED {
            for ord in col.term_ords(doc) {
                buf.clear();
                if col.ord_to_str(ord, &mut buf).unwrap_or(false) {
                    out.insert(buf.clone());
                }
            }
            doc = docset.advance();
        }
    }
    Ok(out.into_iter().collect())
}
