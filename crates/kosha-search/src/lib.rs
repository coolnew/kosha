// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Deterministic intent extraction and bounded, cancellable arena search.
//! No filesystem access, ambient clock, network service, or per-node strings.

mod builtins;
mod calendar;
mod index;
mod numbers;
mod recipe_rules;
mod recipes;
mod translate;

pub use builtins::{BUILTIN_RECIPE_GROUPS, RecipeGroup};
pub use recipes::{
    MAX_RECIPE_BYTES, RECIPE_CAPABILITIES, RECIPE_FILE, Recipe, RecipeBook, RecipePackMetadata,
    RecipeSuggestion,
};

pub use index::{CreationCoverage, SearchIndex};
pub use translate::{
    Diagnostic, Entity, EntityKind, SearchContext, SearchMode, Translation, translate,
    translate_with_mode, translate_with_resolver,
};
