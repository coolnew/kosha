// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! User-defined whole-phrase translations. Parsing and resolution are pure;
//! frontends own file loading, diagnostics, and configuration locations.

use crate::recipe_rules::{CompiledRule, RuleDocument, failure};
use crate::{Entity, EntityKind, SearchContext, SearchMode, Translation, translate_with_resolver};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

pub const RECIPE_FILE: &str = "search-recipes.json";
pub const MAX_RECIPE_BYTES: usize = 1024 * 1024;
const MAX_ACTIVE_RULES: usize = 8192;
const MAX_ACTIVE_PATTERNS: usize = 128;
const MAX_ACTIVE_KEYS: usize = 32768;
const MAX_FUZZY_KEYS: usize = 1024;
pub const RECIPE_CAPABILITIES: &[&str] = &[
    "recipe.exact",
    "recipe.template",
    "recipe.regex",
    "recipe.fuzzy-suggestions",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub phrase: String,
    pub dsl: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipeSuggestion {
    pub phrase: String,
    pub distance: usize,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecipePackMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    pub id: String,
    pub title: String,
    pub description: String,
    pub version: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackDocument {
    schema: u32,
    kind: PackKind,
    metadata: RecipePackMetadata,
    recipes: Vec<RuleDocument>,
    #[serde(default, deserialize_with = "deserialize_requires")]
    requires: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum PackKind {
    RecipePack,
}
fn deserialize_requires<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<String>>, D::Error> {
    Vec::<String>::deserialize(deserializer).map(Some)
}

impl RecipePackMetadata {
    pub fn qualified_id(&self) -> String {
        self.publisher.as_ref().map_or_else(
            || self.id.clone(),
            |publisher| format!("{publisher}/{}", self.id),
        )
    }
    pub fn validate(&self) -> Result<(), String> {
        let text = |value: &str, max| {
            !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
        };
        if !text(&self.id, 80)
            || !self
                .id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            || !text(&self.title, 120)
            || !text(&self.description, 1024)
            || !text(&self.version, 64)
            || !self.version.as_bytes()[0].is_ascii_alphanumeric()
            || !self
                .version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
            || self.publisher.as_ref().is_some_and(|publisher| {
                !text(publisher, 80)
                    || !publisher
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            })
            || self.tags.len() > 32
            || self.tags.iter().any(|tag| !text(tag, 64))
        {
            return Err("Invalid recipe pack metadata.".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RecipeOs {
    Linux,
    Windows,
    Macos,
}

impl RecipeOs {
    fn name(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Windows => "windows",
            Self::Macos => "macos",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RecipeBook {
    recipes: Vec<Recipe>,
    keys: Vec<String>,
    metadata: Option<RecipePackMetadata>,
    fallback_terms: Vec<(usize, String)>,
    rules: Vec<CompiledRule>,
    exact: HashMap<String, Option<usize>>,
    patterns: Vec<usize>,
    fuzzy_terms: Vec<FuzzyTerm>,
    capabilities: Vec<&'static str>,
}

#[derive(Clone, Debug)]
struct FuzzyTerm {
    recipe: usize,
    chars: Vec<char>,
    words: usize,
}

fn check_budget<'a>(rules: impl Iterator<Item = &'a CompiledRule>) -> Result<(), String> {
    let (mut count, mut patterns, mut keys, mut fuzzy) = (0, 0, 0, 0);
    for rule in rules {
        count += 1;
        patterns += usize::from(rule.uses_pattern());
        keys += rule.keys().len();
        if rule.fuzzy {
            fuzzy += rule.keys().len();
        }
    }
    if count > MAX_ACTIVE_RULES
        || patterns > MAX_ACTIVE_PATTERNS
        || keys > MAX_ACTIVE_KEYS
        || fuzzy > MAX_FUZZY_KEYS
    {
        return Err(format!(
            "Active recipe budget exceeded: at most {MAX_ACTIVE_RULES} rules, {MAX_ACTIVE_PATTERNS} template/regex rules, {MAX_ACTIVE_KEYS} phrases/aliases and {MAX_FUZZY_KEYS} fuzzy phrases/aliases across all sources. Remove a pack or simplify recipes."
        ));
    }
    Ok(())
}

pub(crate) fn key(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub(crate) fn as_recipe(mut translation: Translation, input: &str, phrase: &str) -> Translation {
    translation.mode = SearchMode::Natural;
    translation.entities = vec![Entity {
        kind: EntityKind::Recipe,
        span: 0..input.len(),
        value: phrase.into(),
    }];
    translation
}

pub(crate) fn builtin(input: &str) -> Option<(&'static str, &'static str)> {
    if crate::BUILTIN_RECIPE_GROUPS.is_empty() {
        return None;
    }
    let normalized = key(input);
    crate::BUILTIN_RECIPE_GROUPS
        .iter()
        .flat_map(|group| group.recipes.iter().copied())
        .find(|(phrase, _)| *phrase == normalized)
}

impl RecipeBook {
    /// Validate the entire document; a bad entry never produces a partial book.
    pub fn from_json(text: &str) -> Result<Self, String> {
        Self::for_os(text, std::env::consts::OS)
    }

    fn for_os(text: &str, os: &str) -> Result<Self, String> {
        if text.len() > MAX_RECIPE_BYTES {
            return Err("Recipe file exceeds 1 MiB.".into());
        }
        let (input, metadata, requires) = if text.trim_start().starts_with('{') {
            let pack: PackDocument = serde_json::from_str(text).map_err(|e| e.to_string())?;
            if pack.schema != 1 {
                return Err(format!("Unsupported recipe pack schema: {}", pack.schema));
            }
            let PackKind::RecipePack = pack.kind;
            if let Some(required) = pack
                .requires
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .find(|required| !RECIPE_CAPABILITIES.contains(&required.as_str()))
            {
                return Err(format!(
                    "Unsupported required recipe capability: {required}"
                ));
            }
            pack.metadata.validate()?;
            (pack.recipes, Some(pack.metadata), pack.requires)
        } else {
            (
                serde_json::from_str(text).map_err(|e| e.to_string())?,
                None,
                None,
            )
        };
        if input.len() > 256 {
            return Err("Use at most 256 search recipes.".into());
        }
        let mut recipes = Vec::new();
        let mut keys = Vec::new();
        let mut rules = Vec::new();
        let mut pattern_count = 0usize;
        let mut capabilities = Vec::new();
        let mut seen = HashMap::<String, u8>::new();
        for (i, recipe) in input.into_iter().enumerate() {
            let compiled =
                CompiledRule::compile(&recipe).map_err(|e| format!("Recipe {}: {e}", i + 1))?;
            if !capabilities.contains(&compiled.capability()) {
                capabilities.push(compiled.capability());
            }
            if compiled.uses_pattern() {
                pattern_count += 1;
                if pattern_count > 64 {
                    return Err(
                        "Use at most 64 combined template/regex rules per recipe document.".into(),
                    );
                }
            }
            if let Some(requires) = &requires
                && compiled.capability() != "recipe.exact"
                && !requires.iter().any(|r| r == compiled.capability())
            {
                return Err(format!(
                    "Recipe {} uses undeclared required capability {}",
                    i + 1,
                    compiled.capability()
                ));
            }
            let normalized = key(&recipe.phrase);
            if normalized.is_empty()
                || recipe.phrase.len() > 256
                || recipe.phrase.chars().any(char::is_control)
            {
                return Err(format!(
                    "Recipe {} needs a nonempty phrase of at most 256 bytes without control characters.",
                    i + 1
                ));
            }
            if recipe.dsl.trim().is_empty() || recipe.dsl.len() > 16384 {
                return Err(format!("Recipe {} needs DSL of 1 to 16384 bytes.", i + 1));
            }
            let platform_mask = match recipe.os {
                None => 7,
                Some(RecipeOs::Linux) => 1,
                Some(RecipeOs::Windows) => 2,
                Some(RecipeOs::Macos) => 4,
            };
            if compiled.keys().iter().any(|candidate| {
                seen.get(candidate)
                    .is_some_and(|mask| mask & platform_mask != 0)
            }) {
                return Err(format!("Duplicate recipe phrase: {}", recipe.phrase));
            }
            let platform = recipe.os;
            if platform.is_none_or(|platform| platform.name() == os) {
                keys.push(normalized.clone());
                recipes.push(compiled.sample());
            }
            for key in compiled.keys() {
                seen.entry(key.clone())
                    .and_modify(|mask| *mask |= platform_mask)
                    .or_insert(platform_mask);
            }
            if platform.is_none_or(|platform| platform.name() == os) {
                rules.push(compiled);
            }
        }
        check_budget(rules.iter())?;
        let mut book = Self {
            recipes,
            keys,
            metadata,
            fallback_terms: Vec::new(),
            rules,
            capabilities,
            ..Self::default()
        };
        book.index_rules(0);
        Ok(book)
    }

    fn index_rules(&mut self, start: usize) {
        for (index, rule) in self.rules.iter().enumerate().skip(start) {
            if rule.uses_pattern() {
                self.patterns.push(index);
            } else {
                for key in rule.keys() {
                    // Preserve ambiguity when distinct retained rules share an alias.
                    self.exact
                        .entry(key.clone())
                        .and_modify(|entry| *entry = None)
                        .or_insert(Some(index));
                    if rule.fuzzy && fuzzy_safe(key) {
                        self.fuzzy_terms.push(FuzzyTerm {
                            recipe: index,
                            chars: key.chars().collect(),
                            words: key.split_whitespace().count(),
                        });
                    }
                }
            }
        }
    }

    pub fn recipes(&self) -> &[Recipe] {
        &self.recipes
    }

    pub fn metadata(&self) -> Option<&RecipePackMetadata> {
        self.metadata.as_ref()
    }
    /// Features used anywhere in the document, including inactive OS rules.
    pub fn required_capabilities(&self) -> Vec<&'static str> {
        RECIPE_CAPABILITIES
            .iter()
            .copied()
            .filter(|capability| self.capabilities.contains(capability))
            .collect()
    }

    /// Extend an already validated book. Earlier primary phrases retain precedence;
    /// conflicting aliases remain ambiguous. Budget failure leaves this book intact.
    pub fn append_fallback(&mut self, fallback: Self) -> Result<(), String> {
        let start = self.recipes.len();
        let keys: HashSet<_> = self.keys.iter().collect();
        let incoming: Vec<_> = fallback
            .keys
            .into_iter()
            .zip(fallback.recipes)
            .zip(fallback.rules)
            .filter(|((key, _), _)| !keys.contains(key))
            .collect();
        check_budget(
            self.rules
                .iter()
                .chain(incoming.iter().map(|(_, rule)| rule)),
        )?;
        for capability in fallback.capabilities {
            if !self.capabilities.contains(&capability) {
                self.capabilities.push(capability);
            }
        }
        for ((key, recipe), rule) in incoming {
            self.keys.push(key);
            self.recipes.push(recipe);
            self.rules.push(rule);
        }
        if self.recipes.len() != start {
            self.fallback_terms
                .push((start, pack_terms(fallback.metadata.as_ref())));
        }
        self.index_rules(start);
        Ok(())
    }

    pub fn matching_recipes(&self, filter: &str) -> impl Iterator<Item = &Recipe> {
        let filter = key(filter);
        let personal = pack_terms(self.metadata());
        let mut metadata = personal.as_str();
        let mut fallbacks = self.fallback_terms.iter().peekable();
        let mut direct = Vec::new();
        let mut through_pack = Vec::new();
        for (index, recipe) in self.recipes.iter().enumerate() {
            if fallbacks.peek().is_some_and(|(start, _)| *start == index)
                && let Some((_, terms)) = fallbacks.next()
            {
                metadata = terms;
            }
            let entry = key(&format!("{} {}", recipe.phrase, recipe.dsl));
            if filter.split_whitespace().all(|word| entry.contains(word)) {
                direct.push(recipe);
            } else if filter
                .split_whitespace()
                .all(|word| entry.contains(word) || metadata.contains(word))
            {
                through_pack.push(recipe);
            }
        }
        direct.into_iter().chain(through_pack)
    }

    /// Aliases override built-in Natural phrases only. DSL and literal mode
    /// always retain their explicit meaning. No substring substitution occurs.
    pub fn translate_with_resolver(
        &self,
        input: &str,
        context: SearchContext,
        mode: SearchMode,
        resolve_date: impl Fn([i32; 3]) -> Option<u64>,
    ) -> Translation {
        if mode == SearchMode::Natural && input.len() <= 16384 {
            let normalized = key(input);
            let exact = match self.exact.get(&normalized) {
                Some(Some(index)) => Some(*index),
                Some(None) => {
                    return failure(
                        input,
                        0..input.len(),
                        "Ambiguous recipe: multiple whole-phrase rules match this input.".into(),
                    );
                }
                None => None,
            };
            let mut matches = exact
                .into_iter()
                .chain(self.patterns.iter().copied())
                .filter_map(|index| {
                    self.rules[index].translate_normalized(
                        input,
                        &normalized,
                        context,
                        &resolve_date,
                    )
                });
            if let Some(result) = matches.next() {
                if matches.next().is_some() {
                    return failure(
                        input,
                        0..input.len(),
                        "Ambiguous recipe: multiple whole-phrase rules match this input.".into(),
                    );
                }
                return result;
            }
            let mut translated = translate_with_resolver(input, context, mode, &resolve_date);
            let implicit_literal = translated.entities.len() == 1
                && translated.entities[0].kind == EntityKind::Literal
                && !input.trim_start().starts_with(['\'', '"']);
            if !translated.is_valid() || implicit_literal {
                let suggestions = self.fuzzy_suggestions(input);
                if !suggestions.is_empty() {
                    translated.query = None;
                    translated.dsl = None;
                    translated.diagnostics.push(crate::Diagnostic {
                        span: 0..input.len(),
                        message:
                            "Recipe suggestions are available; no corrected query was executed."
                                .into(),
                        hint: format!(
                            "Did you mean: {}? Choose Name mode to search the original text literally.",
                            suggestions
                                .iter()
                                .map(|s| s.phrase.as_str())
                                .collect::<Vec<_>>()
                                .join("; ")
                        ),
                    });
                }
            }
            return translated;
        }
        translate_with_resolver(input, context, mode, resolve_date)
    }
    pub fn fuzzy_suggestions(&self, input: &str) -> Vec<RecipeSuggestion> {
        if input.len() > 16384 {
            return vec![];
        }
        let normalized = key(input);
        if !fuzzy_safe(&normalized) {
            return vec![];
        }
        let chars: Vec<_> = normalized.chars().collect();
        let words = normalized.split_whitespace().count();
        let mut distances = HashMap::<usize, usize>::new();
        for term in &self.fuzzy_terms {
            if term.words == words
                && let Some(distance) = edit_distance(&chars, &term.chars, 2)
            {
                distances
                    .entry(term.recipe)
                    .and_modify(|d| *d = (*d).min(distance))
                    .or_insert(distance);
            }
        }
        let mut result: Vec<_> = distances
            .into_iter()
            .filter(|(_, distance)| *distance > 0)
            .map(|(recipe, distance)| RecipeSuggestion {
                phrase: self.recipes[recipe].phrase.clone(),
                distance,
            })
            .collect();
        result.sort_by(|a, b| a.distance.cmp(&b.distance).then(a.phrase.cmp(&b.phrase)));
        result.dedup_by(|a, b| a.phrase == b.phrase);
        result.truncate(5);
        result
    }
}

fn fuzzy_safe(s: &str) -> bool {
    s.len() >= 4
        && s.len() <= 256
        && !s
            .chars()
            .any(|c| c.is_ascii_digit() || "=<>~!/\\:'\"".contains(c))
        && !s.split_whitespace().any(|w| {
            matches!(
                w,
                "named"
                    | "name"
                    | "path"
                    | "in"
                    | "under"
                    | "containing"
                    | "contains"
                    | "and"
                    | "or"
                    | "not"
                    | "greater"
                    | "less"
                    | "older"
                    | "newer"
                    | "over"
                    | "above"
                    | "below"
                    | "at"
                    | "most"
                    | "least"
            ) || crate::numbers::parse(std::iter::once(w)).is_some()
        })
}

fn edit_distance(mut a: &[char], mut b: &[char], limit: usize) -> Option<usize> {
    if a.len().abs_diff(b.len()) > limit {
        return None;
    }
    // Shared prefixes/suffixes cannot contribute to Levenshtein distance.
    while !a.is_empty() && !b.is_empty() && a[0] == b[0] {
        a = &a[1..];
        b = &b[1..];
    }
    while !a.is_empty() && !b.is_empty() && a.last() == b.last() {
        a = &a[..a.len() - 1];
        b = &b[..b.len() - 1];
    }
    if a.is_empty() || b.is_empty() {
        let distance = a.len().max(b.len());
        return (distance <= limit).then_some(distance);
    }
    let unreachable = limit + 1;
    let mut previous = vec![unreachable; b.len() + 1];
    let mut row = vec![unreachable; b.len() + 1];
    for (j, value) in previous.iter_mut().enumerate().take(limit + 1) {
        *value = j;
    }
    for i in 1..=a.len() {
        let lo = i.saturating_sub(limit).max(1);
        let hi = (i + limit).min(b.len());
        row[0] = if i <= limit { i } else { unreachable };
        if lo > 1 {
            row[lo - 1] = unreachable;
        }
        let mut best = row[0];
        for j in lo..=hi {
            row[j] = (row[j - 1] + 1)
                .min(previous[j] + 1)
                .min(previous[j - 1] + usize::from(a[i - 1] != b[j - 1]));
            best = best.min(row[j]);
        }
        if best > limit {
            return None;
        }
        if hi < b.len() {
            row[hi + 1] = unreachable;
        }
        std::mem::swap(&mut previous, &mut row);
    }
    (previous[b.len()] <= limit).then_some(previous[b.len()])
}

fn pack_terms(metadata: Option<&RecipePackMetadata>) -> String {
    metadata.map_or_else(String::new, |pack| {
        key(&format!(
            "{} {} {} {}",
            pack.qualified_id(),
            pack.title,
            pack.description,
            pack.tags.join(" ")
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const JSON: &str = r#"[{"phrase":"My build files","dsl":"file and ext in (obj,pdb)"}]"#;

    #[test]
    fn aliases_are_whole_phrases_and_explicit_modes_bypass_them() {
        let book = RecipeBook::from_json(JSON).unwrap();
        let context = SearchContext::new(1000, [1970, 1, 1]);
        let t = book.translate_with_resolver(
            "  MY \u{20}\u{20}build files  ",
            context,
            SearchMode::Natural,
            |_| None,
        );
        assert!(t.is_valid());
        assert_eq!(t.dsl.as_deref(), Some("file and ext in (obj,pdb)"));
        assert_eq!(t.entities[0].kind, EntityKind::Recipe);
        assert_eq!(t.entities[0].span, 0..20);
        assert!(
            !book
                .translate_with_resolver(
                    "my build files and images",
                    context,
                    SearchMode::Natural,
                    |_| None
                )
                .is_valid()
        );
        assert!(
            !book
                .translate_with_resolver("my build files", context, SearchMode::Dsl, |_| None)
                .is_valid()
        );
        let literal =
            book.translate_with_resolver("my build files", context, SearchMode::Literal, |_| None);
        assert!(literal.is_valid());
        assert_ne!(literal.dsl, t.dsl);
        assert!(
            book.translate_with_resolver("images", context, SearchMode::Natural, |_| None)
                .is_valid()
        );
    }

    #[test]
    fn malformed_or_ambiguous_books_are_rejected_as_a_whole() {
        for text in [
            r#"[{"phrase":"x","dsl":"file"},{"phrase":" X ","dsl":"dir"}]"#,
            r#"[{"phrase":" ","dsl":"file"}]"#,
            r#"[{"phrase":"x","dsl":"size >"}]"#,
            r#"[{"phrase":"x","dsl":"file","typo":true}]"#,
            r#"[{"phrase":"x","dsl":""}]"#,
            "{}",
        ] {
            assert!(RecipeBook::from_json(text).is_err(), "{text}");
        }
        assert!(RecipeBook::from_json(&" ".repeat(MAX_RECIPE_BYTES + 1)).is_err());
        assert!(RecipeBook::from_json("[]").unwrap().recipes().is_empty());
    }
}

#[cfg(test)]
#[test]
fn banded_unicode_distance_agrees_with_independent_full_matrix() {
    fn reference(a: &[char], b: &[char]) -> usize {
        let mut matrix = vec![vec![0; b.len() + 1]; a.len() + 1];
        for (i, row) in matrix.iter_mut().enumerate() {
            row[0] = i;
        }
        for (j, value) in matrix[0].iter_mut().enumerate() {
            *value = j;
        }
        for i in 1..=a.len() {
            for j in 1..=b.len() {
                matrix[i][j] = (matrix[i - 1][j] + 1)
                    .min(matrix[i][j - 1] + 1)
                    .min(matrix[i - 1][j - 1] + usize::from(a[i - 1] != b[j - 1]));
            }
        }
        matrix[a.len()][b.len()]
    }
    let mut strings = vec![Vec::new()];
    for len in 1..=4 {
        let prior: Vec<_> = strings
            .iter()
            .filter(|s| s.len() == len - 1)
            .cloned()
            .collect();
        for s in prior {
            for c in ['a', 'é', '🦀'] {
                let mut next = s.clone();
                next.push(c);
                strings.push(next);
            }
        }
    }
    strings.extend(
        [
            "abababababab",
            "babababababa",
            "prefixé🦀suffix",
            "prefix🦀ésuffix",
            "aaaaaaaaab",
            "baaaaaaaaa",
        ]
        .map(|s| s.chars().collect()),
    );
    for a in &strings {
        for b in &strings {
            let expected = reference(a, b);
            for limit in 0..=2 {
                assert_eq!(
                    edit_distance(a, b, limit),
                    (expected <= limit).then_some(expected),
                    "{a:?} {b:?} {limit}"
                );
            }
        }
    }
}

#[cfg(test)]
#[test]
fn capabilities_include_inactive_platform_rules() {
    let doc = r#"[{"phrase":"linux items","os":"linux","dsl":"file"},{"type":"template","phrase":"windows items over 1 MB","os":"windows","template":"windows items over {n:size}","dsl":"size > {n}"}]"#;
    let book = RecipeBook::for_os(doc, "linux").unwrap();
    assert_eq!(book.recipes().len(), 1);
    assert_eq!(
        book.required_capabilities(),
        ["recipe.exact", "recipe.template"]
    );
}

#[cfg(test)]
#[test]
fn mixed_platform_recipes_select_only_matching_entries() {
    let json = r#"[
        {"phrase":"tool builds","os":"linux","dsl":"file and ext = o"},
        {"phrase":"Tool Builds","os":"windows","dsl":"file and ext = obj"},
        {"phrase":"shared files","dsl":"file and ext = txt"}
    ]"#;
    let linux = RecipeBook::for_os(json, "linux").unwrap();
    let windows = RecipeBook::for_os(json, "windows").unwrap();
    let macos = RecipeBook::for_os(json, "macos").unwrap();
    assert_eq!(linux.recipes()[0].dsl, "file and ext = o");
    assert_eq!(windows.recipes()[0].dsl, "file and ext = obj");
    assert_eq!(macos.recipes().len(), 1);
    assert_eq!(macos.recipes()[0].phrase, "shared files");
    assert!(
        RecipeBook::for_os(
            r#"[{"phrase":"same","dsl":"file"},{"phrase":"SAME","os":"windows","dsl":"dir"}]"#,
            "linux"
        )
        .is_err()
    );
    assert!(
        RecipeBook::for_os(
            r#"[{"phrase":"platform typo","os":"win","dsl":"file"}]"#,
            "linux"
        )
        .is_err()
    );
}

#[cfg(test)]
#[test]
fn versioned_pack_keeps_metadata_and_validates_its_kind_and_schema() {
    let document = r#"{
        "schema":1,"kind":"recipe-pack",
        "metadata":{"id":"db-tools","title":"Database tools","description":"Find database files","version":"1.0.0","tags":["postgresql"]},
        "recipes":[
            {"phrase":"WAL files","os":"linux","dsl":"file and path ~ pg_wal"},
            {"phrase":"PostgreSQL configuration","os":"linux","dsl":"file and name = postgresql.conf"}
        ]
    }"#;
    let book = RecipeBook::for_os(document, "windows").unwrap();
    assert_eq!(book.metadata().unwrap().id, "db-tools");
    assert!(book.recipes().is_empty());
    let book = RecipeBook::for_os(document, "linux").unwrap();
    assert_eq!(book.recipes()[0].phrase, "WAL files");
    assert_eq!(book.matching_recipes("PostgreSQL WAL").count(), 1);
    assert_eq!(
        book.matching_recipes("postgresql").next().unwrap().phrase,
        "PostgreSQL configuration"
    );
    assert_eq!(book.matching_recipes("sqlite").count(), 0);
    assert!(RecipeBook::from_json(&document.replace("\"schema\":1", "\"schema\":2")).is_err());
    assert!(RecipeBook::from_json(&document.replace("recipe-pack", "wasi-extension")).is_err());
    assert!(RecipeBook::from_json(&document.replace("db-tools", "../db-tools")).is_err());
}
