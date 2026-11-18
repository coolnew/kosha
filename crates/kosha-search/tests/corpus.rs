// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use kosha_search::{SearchContext, SearchMode, translate_with_mode};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    input: String,
    dsl: Option<String>,
    #[serde(default)]
    entities: Vec<ExpectedEntity>,
    diagnostic: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedEntity {
    kind: String,
    span: [usize; 2],
    text: String,
    value: String,
}

#[test]
fn natural_language_corpus_entities_and_exact_dsl() {
    let mut ids = std::collections::HashSet::new();
    for (line, json) in include_str!("corpus.jsonl").lines().enumerate() {
        let case: Case = serde_json::from_str(json).expect("valid corpus JSONL");
        assert!(
            !case.id.is_empty() && ids.insert(case.id.clone()),
            "Unique nonempty case ID required"
        );
        assert_ne!(
            case.dsl.is_some(),
            case.diagnostic.is_some(),
            "{} must specify exactly one outcome",
            case.id
        );
        assert!(
            case.diagnostic.as_ref().is_none_or(|s| !s.is_empty()),
            "{} nonempty diagnostic required",
            case.id
        );
        let result = translate_with_mode(
            &case.input,
            SearchContext::new(1795003200, [2026, 11, 18]),
            SearchMode::Natural,
        );
        if let Some(fragment) = case.diagnostic {
            assert!(
                result.dsl.is_none(),
                "{} must not expose executable DSL",
                case.id
            );
            assert!(
                result.query.is_none(),
                "line {} must not execute: {}",
                line + 1,
                case.input
            );
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|d| d.message.contains(&fragment)),
                "line {}: {:?}",
                line + 1,
                result.diagnostics
            );
            continue;
        }
        assert_eq!(
            result.dsl,
            case.dsl,
            "line {}: {} diagnostics={:?}",
            line + 1,
            case.input,
            result.diagnostics
        );
        assert!(result.is_valid(), "line {}", line + 1);
        assert_eq!(
            result.entities.len(),
            case.entities.len(),
            "line {}",
            line + 1
        );
        for (actual, expected) in result.entities.iter().zip(case.entities) {
            assert_eq!(format!("{:?}", actual.kind), expected.kind, "{}", case.id);
            assert_eq!(
                [actual.span.start, actual.span.end],
                expected.span,
                "{} exact byte span",
                case.id
            );
            assert_eq!(
                &case.input[actual.span.clone()],
                expected.text,
                "line {} byte span",
                line + 1
            );
            assert_eq!(actual.value, expected.value, "{}", case.id);
        }
    }
}
