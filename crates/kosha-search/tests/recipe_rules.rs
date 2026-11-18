// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use kosha_search::{RECIPE_CAPABILITIES, RecipeBook, SearchContext, SearchMode};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    recipes: serde_json::Value,
    input: Option<String>,
    dsl: Option<String>,
    error: Option<String>,
    load_error: Option<String>,
    hint: Option<String>,
    mode: Option<String>,
    #[serde(default)]
    captures: Vec<[String; 3]>,
    #[serde(default)]
    valid_builtin: bool,
}
fn context() -> SearchContext {
    SearchContext::new(1795003200, [2026, 11, 18])
}

#[test]
fn indexed_exact_aliases_preserve_precedence_and_all_match_ambiguity() {
    let mut book =
        RecipeBook::from_json(r#"[{"phrase":"primary","aliases":["shared"],"dsl":"file"}]"#)
            .unwrap();
    book.append_fallback(RecipeBook::from_json(r#"[{"phrase":"primary","dsl":"dir"},{"phrase":"secondary","aliases":["shared"],"dsl":"dir"}]"#).unwrap()).unwrap();
    assert_eq!(
        book.translate_with_resolver("primary", context(), SearchMode::Natural, |_| None)
            .dsl
            .as_deref(),
        Some("file")
    );
    let ambiguous =
        book.translate_with_resolver("shared", context(), SearchMode::Natural, |_| None);
    assert!(ambiguous.query.is_none());
    assert!(ambiguous.diagnostics[0].message.contains("Ambiguous"));
    let mut book = RecipeBook::from_json(r#"[{"phrase":"amount nope","dsl":"file"}]"#).unwrap();
    book.append_fallback(RecipeBook::from_json(r#"[{"type":"template","phrase":"amount 1 MB","template":"amount {n:size}","dsl":"size > {n}"}]"#).unwrap()).unwrap();
    // A whole-phrase typed match counts even if binding fails.
    let result =
        book.translate_with_resolver("amount nope", context(), SearchMode::Natural, |_| None);
    assert!(result.query.is_none());
    assert!(result.diagnostics[0].message.contains("Ambiguous"));
}

#[test]
fn aggregate_rule_pattern_alias_and_fuzzy_budgets_fail_atomically() {
    let mut book = RecipeBook::default();
    for pack in 0..2 {
        let rules:Vec<_>=(0..64).map(|i|json!({"type":"template","phrase":format!("batch{pack} rule{i} over 1 MB"),"template":format!("batch{pack} rule{i} over {{n:size}}"),"dsl":"size > {n}"})).collect();
        book.append_fallback(RecipeBook::from_json(&json!(rules).to_string()).unwrap())
            .unwrap();
    }
    let extra=RecipeBook::from_json(r#"[{"type":"template","phrase":"extra over 1 MB","template":"extra over {n:size}","dsl":"size > {n}"}]"#).unwrap();
    assert!(book.append_fallback(extra).unwrap_err().contains("budget"));
    assert_eq!(book.recipes().len(), 128);
    assert!(
        book.translate_with_resolver(
            "batch1 rule63 over 2 MB",
            context(),
            SearchMode::Natural,
            |_| None
        )
        .is_valid()
    );
    let mut aliases = RecipeBook::default();
    for pack in 0..4 {
        let rules:Vec<_>=(0..256).map(|i|json!({"phrase":format!("batch{pack} rule{i}"),"aliases":(0..32).map(|j|format!("alias{pack} rule{i} option{j}")).collect::<Vec<_>>(),"dsl":"file"})).collect();
        let result =
            aliases.append_fallback(RecipeBook::from_json(&json!(rules).to_string()).unwrap());
        assert_eq!(result.is_ok(), pack < 3);
    }
    assert_eq!(aliases.recipes().len(), 768);
    let fuzzy:Vec<_>=(0..32).map(|i|json!({"type":"fuzzy-suggestions","phrase":format!("fuzzy{i}"),"aliases":(0..32).map(|j|format!("alias{i} option{j}")).collect::<Vec<_>>(),"dsl":"file"})).collect();
    assert!(
        RecipeBook::from_json(&json!(fuzzy).to_string())
            .unwrap_err()
            .contains("budget")
    );
    let mut exact = RecipeBook::default();
    for pack in 0..32 {
        let rules: Vec<_> = (0..256)
            .map(|i| json!({"phrase":format!("batch{pack} rule{i}"),"dsl":"file"}))
            .collect();
        exact
            .append_fallback(RecipeBook::from_json(&json!(rules).to_string()).unwrap())
            .unwrap();
    }
    assert!(
        exact
            .append_fallback(RecipeBook::from_json(r#"[{"phrase":"extra","dsl":"dir"}]"#).unwrap())
            .is_err()
    );
    assert_eq!(exact.recipes().len(), 8192);
    // Fully suppressed fallback rules consume no additional active budget.
    exact
        .append_fallback(
            RecipeBook::from_json(r#"[{"phrase":"batch0 rule0","dsl":"dir"}]"#).unwrap(),
        )
        .unwrap();
}

#[test]
fn typed_output_budget_prevents_capture_amplification_before_rendering() {
    let book=RecipeBook::from_json(&json!([{"type":"template","phrase":"review notes","template":"review {text:text}","dsl":std::iter::repeat_n("name ~ {text}",64).collect::<Vec<_>>().join(" and ")}]).to_string()).unwrap();
    let input = format!("review {}", "a".repeat(1024));
    let result = book.translate_with_resolver(&input, context(), SearchMode::Natural, |_| None);
    assert!(result.query.is_none());
    assert!(result.dsl.is_none());
    assert!(result.diagnostics[0].message.contains("output budget"));
    assert!(
        book.translate_with_resolver("review literal", context(), SearchMode::Natural, |_| None)
            .is_valid()
    );
    let membership=RecipeBook::from_json(r#"[{"type":"template","phrase":"review notes","template":"review {text:text}","dsl":"name in ({text}, other)"}]"#).unwrap();
    let result = membership.translate_with_resolver(
        &format!("review {}", "a".repeat(16300)),
        context(),
        SearchMode::Natural,
        |_| None,
    );
    assert!(result.is_valid());
    assert!(result.dsl.unwrap().len() <= 16384);
    for dsl in [
        format!(
            "name not in ({})",
            std::iter::repeat_n("{text}", 64)
                .collect::<Vec<_>>()
                .join(",")
        ),
        std::iter::repeat_n("not (name startswith {text} or name = other)", 32)
            .collect::<Vec<_>>()
            .join(" and "),
    ] {
        let book=RecipeBook::from_json(&json!([{"type":"template","phrase":"review notes","template":"review {text:text}","dsl":dsl}]).to_string()).unwrap();
        // Bound bytes, not Unicode scalar count; the author can repeat slots.
        let result = book.translate_with_resolver(
            &format!("review {}", "🦀".repeat(256)),
            context(),
            SearchMode::Natural,
            |_| None,
        );
        assert!(result.query.is_none());
        assert!(result.diagnostics[0].message.contains("output budget"));
    }
}

#[test]
fn unnamed_captures_share_the_named_capture_resource_limit() {
    let doc = |count| {
        json!([{"type":"regex","phrase":"a".repeat(count),"pattern":"(a)".repeat(count),"dsl":"file"}]).to_string()
    };
    assert!(RecipeBook::from_json(&doc(16)).is_ok());
    assert!(
        RecipeBook::from_json(&doc(17))
            .unwrap_err()
            .contains("16 capture groups")
    );
}

#[test]
fn declarative_rules_jsonl_examples_have_exact_dsl_captures_or_explicit_failure() {
    for line in include_str!("recipe_rules.jsonl").lines() {
        let case: Case = serde_json::from_str(line).unwrap();
        let loaded = RecipeBook::from_json(&case.recipes.to_string());
        if let Some(error) = case.load_error {
            assert!(
                loaded.as_ref().is_err_and(|e| e.contains(&error)),
                "{}: {:?}",
                case.id,
                loaded
            );
            continue;
        }
        let book = loaded.unwrap_or_else(|e| panic!("{}: {e}", case.id));
        assert!(!book.recipes().is_empty());
        let mode = match case.mode.as_deref() {
            Some("dsl") => SearchMode::Dsl,
            Some("literal") => SearchMode::Literal,
            _ => SearchMode::Natural,
        };
        let input = case.input.unwrap();
        let result = book.translate_with_resolver(&input, context(), mode, |_| None);
        if let Some(error) = case.error {
            assert!(
                result.query.is_none() && result.dsl.is_none(),
                "{}",
                case.id
            );
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|d| d.message.contains(&error)),
                "{}: {:?}",
                case.id,
                result.diagnostics
            );
            if let Some(hint) = case.hint {
                assert!(result.diagnostics.iter().any(|d| d.hint.contains(&hint)));
            }
            continue;
        }
        assert!(result.is_valid(), "{}: {:?}", case.id, result.diagnostics);
        if case.valid_builtin {
            assert!(
                result
                    .entities
                    .iter()
                    .all(|e| e.kind != kosha_search::EntityKind::Recipe)
            );
            assert!(result.diagnostics.is_empty());
            continue;
        }
        assert_eq!(result.dsl, case.dsl, "{}", case.id);
        let query = result.query.unwrap();
        let reparsed = kosha_query::Query::parse(query.source()).unwrap();
        if mode != SearchMode::Literal && !input.trim_start().starts_with(['\'', '"']) {
            assert_eq!(query.expression(), reparsed.expression(), "{}", case.id);
        }
        for [kind, text, value] in case.captures {
            assert!(
                result
                    .entities
                    .iter()
                    .any(|entity| format!("{:?}", entity.kind) == kind
                        && input[entity.span.clone()] == text
                        && entity.value == value),
                "{}: {:?}",
                case.id,
                result.entities
            );
        }
    }
}

#[test]
fn metadata_namespaces_required_features_and_bounded_regex_are_checked() {
    let document = |publisher: &str, requires: serde_json::Value, recipes: serde_json::Value| {
        serde_json::json!({"schema":1,"kind":"recipe-pack","metadata":{"publisher":publisher,"id":"tools","title":"Tools","description":"Test tools","version":"1.0"},"requires":requires,"recipes":recipes}).to_string()
    };
    let exact = serde_json::json!([{"phrase":"build files","dsl":"file"}]);
    let book = RecipeBook::from_json(&document(
        "alice",
        serde_json::json!(["recipe.exact"]),
        exact.clone(),
    ))
    .unwrap();
    assert_eq!(book.metadata().unwrap().qualified_id(), "alice/tools");
    assert!(
        RecipeBook::from_json(&document("Alice", serde_json::json!([]), exact.clone())).is_err()
    );
    assert!(
        RecipeBook::from_json(&document(
            "alice",
            serde_json::json!(["recipe.execute-code"]),
            exact
        ))
        .is_err()
    );
    let template = serde_json::json!([{"type":"template","phrase":"over 1 MB","template":"over {minimum:size}","dsl":"size > {minimum}"}]);
    assert!(
        RecipeBook::from_json(&document(
            "alice",
            serde_json::json!(["recipe.exact"]),
            template.clone()
        ))
        .is_err()
    );
    let book = RecipeBook::from_json(&document(
        "alice",
        serde_json::json!(["recipe.template"]),
        template,
    ))
    .unwrap();
    assert_eq!(book.required_capabilities(), vec!["recipe.template"]);
    for capability in [
        "recipe.exact",
        "recipe.template",
        "recipe.regex",
        "recipe.fuzzy-suggestions",
    ] {
        assert!(RECIPE_CAPABILITIES.contains(&capability));
    }
    let oversized =
        serde_json::json!([{"type":"regex","phrase":"x","pattern":"x".repeat(4097),"dsl":"file"}]);
    assert!(RecipeBook::from_json(&oversized.to_string()).is_err());
    let too_large = serde_json::json!([{"type":"regex","phrase":"x","pattern":"(?:x{1000}){1000}","dsl":"file"}]);
    assert!(RecipeBook::from_json(&too_large.to_string()).is_err());
}

#[test]
fn safely_bound_paths_and_text_preserve_literals_and_reject_unrepresentable_quotes() {
    use kosha_query::{Expr, Field, Value};
    let book=RecipeBook::from_json(r#"[{"type":"template","phrase":"inside C:\\cache","template":"inside {folder:path}","dsl":"path ~ {folder}"}]"#).unwrap();
    for literal in [
        r"C:\Users\résumé",
        r"\\server\共有\",
        r#"C:\cache" or dir"#,
        r"C:\O'Brien",
    ] {
        let input = format!("inside {literal}");
        let result = book.translate_with_resolver(&input, context(), SearchMode::Natural, |_| None);
        assert!(result.is_valid(), "{input}: {:?}", result.diagnostics);
        let query = result.query.unwrap();
        let Expr::Compare {
            field: Field::Path,
            value: Value::Text(value),
            ..
        } = query.expression()
        else {
            panic!("path literal was not kept as one AST comparison")
        };
        assert_eq!(value, literal);
        assert_eq!(
            kosha_query::Query::parse(query.source())
                .unwrap()
                .expression(),
            query.expression()
        );
    }
    let result = book.translate_with_resolver(
        "inside C:\\O'Brien\"",
        context(),
        SearchMode::Natural,
        |_| None,
    );
    assert!(result.query.is_none() && result.dsl.is_none());
    assert!(book.fuzzy_suggestions("files from buidl 10 MB").is_empty());
}

#[test]
fn typed_rules_execute_bound_values_without_injection_or_unknown_time_substitution() {
    use kosha_core::{Flags, NameArena, Node, Owners, ScanStats, SizeTree};
    use std::{
        ffi::OsStr,
        sync::{Arc, atomic::AtomicBool},
    };
    let mut names = NameArena::default();
    let specs = [
        ("root", 100, 0, Flags::DIR),
        ("old.bin", 20, 1794398399, Flags::default()),
        ("boundary.bin", 20, 1794398400, Flags::default()),
        ("unknown.bin", 20, 0, Flags::default()),
        ("a\" or dir", 1, 1795003200, Flags::default()),
        ("other", 1, 1795003200, Flags::DIR),
    ];
    let nodes = specs
        .into_iter()
        .enumerate()
        .map(|(i, (name, allocated, mtime, flags))| {
            let (name_off, name_len) = names.push(OsStr::new(name));
            Node {
                allocated,
                logical: allocated,
                parent: 0,
                first_child: if i == 0 { 1 } else { 0 },
                child_count: if i == 0 { 5 } else { 0 },
                subtree_files: 0,
                name_off,
                name_len,
                mtime,
                flags,
                depth: u8::from(i != 0),
            }
        })
        .collect();
    let tree = Arc::new(SizeTree::from_parts(
        nodes,
        names,
        Owners::default(),
        "C:/recipes".into(),
        ScanStats::default(),
    ));
    let index = kosha_search::SearchIndex::new(tree.clone());
    let book=RecipeBook::from_json(r#"[{"type":"template","phrase":"old over 10 bytes beyond 7 days","template":"old over {minimum:size} beyond {age:duration}","dsl":"file and size > {minimum} and age > {age}"},{"type":"template","phrase":"named budget","template":"named {label:text}","dsl":"name = {label}"}]"#).unwrap();
    for (input, expected) in [
        ("old over ten bytes beyond seven days", "old.bin"),
        ("named a\" or dir", "a\" or dir"),
    ] {
        let translated =
            book.translate_with_resolver(input, context(), SearchMode::Natural, |_| None);
        assert!(
            translated.is_valid(),
            "{input}: {:?}",
            translated.diagnostics
        );
        let query = translated.query.unwrap();
        let result = index
            .run(&query, context().now, 100, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(result.total_matched, 1);
        assert_eq!(tree.name_lossy(result.matches[0]), expected);
        let reparsed = kosha_query::Query::parse(query.source()).unwrap();
        for id in (0..tree.len()).map(kosha_core::NodeId::from_index) {
            assert_eq!(
                query.matches(&tree, id, context().now),
                reparsed.matches(&tree, id, context().now)
            );
        }
    }
}

#[test]
fn fuzzy_candidates_never_correct_quantities_operators_or_path_values() {
    let book=RecipeBook::from_json(r#"[{"type":"fuzzy-suggestions","phrase":"files over ten MB","dsl":"file"},{"type":"fuzzy-suggestions","phrase":"files and folders","dsl":"dir"},{"type":"fuzzy-suggestions","phrase":"files in cache","dsl":"file"},{"type":"fuzzy-suggestions","phrase":"files from build","dsl":"file"}]"#).unwrap();
    for input in [
        "files over ton MB",
        "files an folders",
        "files in cahce",
        "files from build 10 MB",
    ] {
        assert!(book.fuzzy_suggestions(input).is_empty(), "{input}");
    }
    let result =
        book.translate_with_resolver("files from buidl", context(), SearchMode::Natural, |_| None);
    assert!(result.query.is_none());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.hint.contains("files from build"))
    );
    let padded = format!("{}files from buidl{}", " ".repeat(256), " ".repeat(256));
    assert_eq!(
        book.fuzzy_suggestions(&padded),
        book.fuzzy_suggestions("files from buidl")
    );
}

#[test]
fn pattern_count_budget_is_separate_from_legacy_exact_recipe_count() {
    let patterns:Vec<_>=(0..65).map(|i|serde_json::json!({"type":"template","phrase":format!("rule{i} over 1 MB"),"template":format!("rule{i} over {{minimum:size}}"),"dsl":"size > {minimum}"})).collect();
    assert!(
        RecipeBook::from_json(&serde_json::to_string(&patterns).unwrap())
            .unwrap_err()
            .contains("64 combined")
    );
    assert_eq!(
        RecipeBook::from_json(&serde_json::to_string(&patterns[..64]).unwrap())
            .unwrap()
            .recipes()
            .len(),
        64
    );
    let exact: Vec<_> = (0..256)
        .map(|i| serde_json::json!({"phrase":format!("exact rule {i}"),"dsl":"file"}))
        .collect();
    assert_eq!(
        RecipeBook::from_json(&serde_json::to_string(&exact).unwrap())
            .unwrap()
            .recipes()
            .len(),
        256
    );
}
