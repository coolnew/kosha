// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Bounded declarative matchers and typed AST binding. Captured user input is
//! never interpolated into executable DSL; only parsed values enter the AST.

use crate::recipes::{RecipeOs, as_recipe, key};
use crate::{
    Diagnostic, Entity, EntityKind, Recipe, SearchContext, SearchMode, Translation,
    translate_with_resolver,
};
use kosha_query::{Expr, Field, Flag, Op, Query, Value};
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use std::{collections::BTreeMap, ops::Range};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum RuleKind {
    #[default]
    Exact,
    Template,
    Regex,
    FuzzySuggestions,
}
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(super) enum BindingType {
    Size,
    Duration,
    Text,
    Path,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RuleDocument {
    pub phrase: String,
    pub dsl: String,
    pub os: Option<RecipeOs>,
    #[serde(default, rename = "type")]
    pub kind: RuleKind,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub template: Option<String>,
    pub pattern: Option<String>,
    #[serde(default)]
    pub bindings: BTreeMap<String, BindingType>,
}
#[derive(Clone, Debug)]
struct Slot {
    name: String,
    kind: BindingType,
    marker: Value,
}
#[derive(Clone, Debug)]
pub(super) struct CompiledRule {
    kind: RuleKind,
    phrase: String,
    dsl: String,
    keys: Vec<String>,
    matcher: Option<Regex>,
    slots: Vec<Slot>,
    ast: Option<Expr>,
    pub fuzzy: bool,
}

fn identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
fn phrase_valid(phrase: &str) -> bool {
    !phrase.trim().is_empty() && phrase.len() <= 256 && !phrase.chars().any(char::is_control)
}

impl CompiledRule {
    pub fn uses_pattern(&self) -> bool {
        self.matcher.is_some()
    }
    pub fn capability(&self) -> &'static str {
        match self.kind {
            RuleKind::Exact => "recipe.exact",
            RuleKind::Template => "recipe.template",
            RuleKind::Regex => "recipe.regex",
            RuleKind::FuzzySuggestions => "recipe.fuzzy-suggestions",
        }
    }
    pub fn compile(doc: &RuleDocument) -> Result<Self, String> {
        if !phrase_valid(&doc.phrase)
            || doc.aliases.len() > 32
            || doc.aliases.iter().any(|a| !phrase_valid(a))
        {
            return Err("Recipe phrases/aliases require 1..256 bytes without control characters (at most 32 aliases).".into());
        }
        if doc.dsl.trim().is_empty() || doc.dsl.len() > 16384 {
            return Err("Recipe DSL requires 1..16384 bytes.".into());
        }
        let mut keys = vec![key(&doc.phrase)];
        for alias in &doc.aliases {
            let alias = key(alias);
            if keys.contains(&alias) {
                return Err("Duplicate alias within a recipe.".into());
            }
            keys.push(alias);
        }
        let mut bindings = doc.bindings.clone();
        let matcher = match doc.kind {
            RuleKind::Exact | RuleKind::FuzzySuggestions => {
                if doc.template.is_some() || doc.pattern.is_some() || !bindings.is_empty() {
                    return Err(
                        "Exact recipes cannot declare a template, pattern, or bindings.".into(),
                    );
                }
                None
            }
            RuleKind::Template => {
                if doc.pattern.is_some() || !bindings.is_empty() || !doc.aliases.is_empty() {
                    return Err(
                        "Templates declare typed placeholders, not pattern/bindings/aliases."
                            .into(),
                    );
                }
                let template = doc
                    .template
                    .as_deref()
                    .ok_or("Template recipe needs template.")?;
                let (pattern, declared) = template_pattern(template)?;
                bindings = declared;
                Some(compile_regex(&pattern, true)?)
            }
            RuleKind::Regex => {
                if doc.template.is_some() || !doc.aliases.is_empty() {
                    return Err(
                        "Regex recipes use pattern and bindings, not template or aliases.".into(),
                    );
                }
                Some(compile_regex(
                    doc.pattern
                        .as_deref()
                        .ok_or("Regex recipe needs pattern.")?,
                    false,
                )?)
            }
        };
        if bindings.len() > 16 || bindings.keys().any(|k| !identifier(k)) {
            return Err("Use at most 16 lowercase named capture bindings.".into());
        }
        if let Some(regex) = &matcher {
            let names: Vec<_> = regex.capture_names().flatten().collect();
            if names.len() != bindings.len() || names.iter().any(|n| !bindings.contains_key(*n)) {
                return Err(
                    "Every named capture must have exactly one declared binding type.".into(),
                );
            }
        }
        let slots: Vec<_> = bindings
            .into_iter()
            .enumerate()
            .map(|(i, (name, kind))| Slot {
                name,
                kind,
                marker: if matches!(kind, BindingType::Text | BindingType::Path) {
                    Value::Text(format!("__kosha_recipe_slot_{i}__"))
                } else {
                    Value::Number(9_000_000_000_000_000_000 + i as u64)
                },
            })
            .collect();
        let ast = if matcher.is_some() {
            Some(compile_ast(&doc.dsl, &slots)?)
        } else {
            Query::parse(&doc.dsl).map_err(|e| format!("Invalid recipe DSL: {}", e.message))?;
            None
        };
        let rule = Self {
            kind: doc.kind,
            phrase: doc.phrase.clone(),
            dsl: doc.dsl.clone(),
            keys,
            matcher,
            slots,
            ast,
            fuzzy: doc.kind == RuleKind::FuzzySuggestions,
        };
        let context = SearchContext::new(1795003200, [2026, 11, 18]);
        match rule.translate(&doc.phrase, context, &|[y, m, d]| {
            u64::try_from(
                kosha_core::format::days_from_civil(i64::from(y), m as u32, d as u32) * 86400,
            )
            .ok()
        }) {
            Some(t) if t.is_valid() => Ok(rule),
            _ => Err(
                "Recipe sample phrase must match its rule and resolve every typed binding.".into(),
            ),
        }
    }
    pub fn keys(&self) -> &[String] {
        &self.keys
    }
    pub fn sample(&self) -> Recipe {
        let context = SearchContext::new(1795003200, [2026, 11, 18]);
        let translation = self
            .translate(&self.phrase, context, &|_| None)
            .expect("validated sample");
        Recipe {
            phrase: self.phrase.clone(),
            dsl: translation.dsl.expect("validated sample DSL"),
        }
    }
    pub fn translate(
        &self,
        input: &str,
        context: SearchContext,
        resolver: &dyn Fn([i32; 3]) -> Option<u64>,
    ) -> Option<Translation> {
        self.translate_normalized(input, &key(input), context, resolver)
    }
    pub fn translate_normalized(
        &self,
        input: &str,
        normalized: &str,
        context: SearchContext,
        resolver: &dyn Fn([i32; 3]) -> Option<u64>,
    ) -> Option<Translation> {
        let Some(matcher) = &self.matcher else {
            return self.keys.iter().any(|key| key == normalized).then(|| {
                as_recipe(
                    translate_with_resolver(&self.dsl, context, SearchMode::Dsl, resolver),
                    input,
                    &self.phrase,
                )
            });
        };
        let captures = matcher.captures(input)?;
        let mut values = vec![];
        let mut entities = vec![Entity {
            kind: EntityKind::Recipe,
            span: 0..input.len(),
            value: self.phrase.clone(),
        }];
        for slot in &self.slots {
            let Some(capture) = captures.name(&slot.name) else {
                return Some(failure(
                    input,
                    0..input.len(),
                    format!("Missing recipe capture: {}", slot.name),
                ));
            };
            let raw = capture.as_str();
            let trimmed = raw.trim();
            let start = capture.start() + raw.len() - raw.trim_start().len();
            let span = start..start + trimmed.len();
            let value = match bind_value(slot.kind, trimmed, context, resolver) {
                Ok(v) => v,
                Err(e) => {
                    return Some(failure(
                        input,
                        span,
                        format!("Recipe capture {}: {e}", slot.name),
                    ));
                }
            };
            entities.push(Entity {
                kind: match slot.kind {
                    BindingType::Size => EntityKind::Size,
                    BindingType::Duration => EntityKind::Age,
                    BindingType::Text => EntityKind::Name,
                    BindingType::Path => EntityKind::Path,
                },
                span,
                value: match &value {
                    Value::Text(t) => t.clone(),
                    Value::Number(n) => format!(
                        "{n}{}",
                        if slot.kind == BindingType::Size {
                            "b"
                        } else {
                            "s"
                        }
                    ),
                },
            });
            values.push(value);
        }
        let ast = self.ast.as_ref().expect("compiled typed AST");
        if !bounded_output(ast, &self.slots, &values) {
            return Some(failure(input, 0..input.len(), "Bound recipe exceeds the 16384-byte output budget; shorten values or simplify the recipe.".into()));
        }
        let mut expr = ast.clone();
        replace(&mut expr, &self.slots, &values);
        let dsl = match render(&expr, 0) {
            Ok(dsl) => dsl,
            Err(e) => return Some(failure(input, 0..input.len(), e)),
        };
        entities[1..].sort_by_key(|e| e.span.start);
        Some(Translation {
            query: Some(Query::from_expression(expr, dsl.clone())),
            dsl: Some(dsl),
            entities,
            diagnostics: vec![],
            mode: SearchMode::Natural,
        })
    }
}

// Conservative serialized-size admission BEFORE cloning/repeating captured
// values. A short template can repeat a 16KiB capture hundreds of times; the
// recursive renderer would otherwise allocate/copy a greatly amplified DSL.
fn bounded_output(expr: &Expr, slots: &[Slot], values: &[Value]) -> bool {
    let mut pending = vec![expr];
    let mut bytes = 0usize;
    let value_bytes = |value: &Value| {
        let value = slots
            .iter()
            .position(|s| s.marker == *value)
            .map_or(value, |index| &values[index]);
        match value {
            Value::Text(text) => text.len().saturating_add(2),
            Value::Number(_) => 22,
        }
    };
    while let Some(expr) = pending.pop() {
        let extra = match expr {
            Expr::And(a, b) | Expr::Or(a, b) => {
                pending.extend([a.as_ref(), b.as_ref()]);
                9
            }
            Expr::Not(inner) => {
                pending.push(inner);
                6
            }
            Expr::Flag(flag) => flag.name().len(),
            Expr::NameContains(text) => text.len().saturating_add(2),
            Expr::Compare { field, value, .. } => field
                .name()
                .len()
                .saturating_add(14)
                .saturating_add(value_bytes(value)),
            Expr::In {
                field,
                values: list,
                ..
            } => list.iter().fold(field.name().len() + 12, |sum, value| {
                sum.saturating_add(value_bytes(value)).saturating_add(2)
            }),
        };
        bytes = bytes.saturating_add(extra);
        if bytes > 16384 {
            return false;
        }
    }
    true
}

fn compile_regex(pattern: &str, case_insensitive: bool) -> Result<Regex, String> {
    if pattern.len() > 4096 {
        return Err("Recipe regex exceeds 4096 bytes.".into());
    }
    let regex = RegexBuilder::new(&format!("\\A(?:{pattern})\\z"))
        .case_insensitive(case_insensitive)
        .size_limit(256 * 1024)
        .dfa_size_limit(256 * 1024)
        .nest_limit(32)
        .build()
        .map_err(|e| format!("Invalid or oversized recipe regex: {e}"))?;
    if regex.captures_len() > 17 {
        return Err("Recipe regex supports at most 16 capture groups, including unnamed groups; use (?:...) for grouping.".into());
    }
    Ok(regex)
}
fn template_pattern(template: &str) -> Result<(String, BTreeMap<String, BindingType>), String> {
    let template = template.trim();
    if template.is_empty() || template.len() > 1024 {
        return Err("Template requires 1..1024 bytes.".into());
    }
    let mut result = String::from("\\s*");
    let mut declared = BTreeMap::new();
    let mut cursor = 0;
    let mut after_slot = false;
    while let Some(open) = template[cursor..].find('{').map(|i| i + cursor) {
        let literal = &template[cursor..open];
        if after_slot && literal.trim().is_empty() {
            return Err("Adjacent placeholders need a non-whitespace literal delimiter.".into());
        }
        result.push_str(&literal_pattern(literal));
        let close = template[open..]
            .find('}')
            .map(|i| i + open)
            .ok_or("Unclosed template placeholder.")?;
        let (name, kind) = template[open + 1..close]
            .split_once(':')
            .ok_or("Template placeholder needs {name:type}.")?;
        let kind = match kind {
            "size" => BindingType::Size,
            "duration" => BindingType::Duration,
            "text" => BindingType::Text,
            "path" => BindingType::Path,
            _ => return Err(format!("Unsupported template binding type: {kind}")),
        };
        if !identifier(name) || declared.insert(name.into(), kind).is_some() {
            return Err("Invalid or duplicate template placeholder name.".into());
        }
        result.push_str(&format!("(?P<{name}>.+?)"));
        cursor = close + 1;
        after_slot = true;
    }
    if declared.is_empty() {
        return Err("Template needs at least one typed placeholder.".into());
    }
    if template[cursor..].contains('}') {
        return Err("Unmatched template brace.".into());
    }
    result.push_str(&literal_pattern(&template[cursor..]));
    result.push_str("\\s*");
    Ok((result, declared))
}
fn literal_pattern(text: &str) -> String {
    let mut out = String::new();
    let mut whitespace = false;
    for c in text.chars() {
        if c.is_whitespace() {
            if !whitespace {
                out.push_str("\\s+");
            }
            whitespace = true;
        } else {
            out.push_str(&regex::escape(&c.to_string()));
            whitespace = false;
        }
    }
    out
}

fn compile_ast(dsl: &str, slots: &[Slot]) -> Result<Expr, String> {
    let mut output = String::new();
    let mut chars = dsl.char_indices().peekable();
    let mut quote = None;
    let mut references = vec![0usize; slots.len()];
    while let Some((_, c)) = chars.next() {
        if c == '\'' || c == '"' {
            if quote == Some(c) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(c);
            }
            output.push(c);
            continue;
        }
        if c != '{' {
            output.push(c);
            continue;
        }
        let mut name = String::new();
        let mut closed = false;
        for (_, next) in chars.by_ref() {
            if next == '}' {
                closed = true;
                break;
            }
            name.push(next);
        }
        if !closed {
            return Err("Unclosed DSL binding placeholder.".into());
        }
        let name_only = name.split(':').next().unwrap_or("");
        let Some((index, slot)) = slots
            .iter()
            .enumerate()
            .find(|(_, slot)| slot.name == name_only)
        else {
            if quote.is_some() {
                output.push('{');
                output.push_str(&name);
                output.push('}');
                continue;
            }
            return Err(format!("Unknown DSL binding: {name}"));
        };
        if quote.is_some() {
            return Err("DSL placeholders must be unquoted complete values; text/path values are quoted automatically.".into());
        }
        if let Some((_, kind)) = name.split_once(':')
            && kind
                != match slot.kind {
                    BindingType::Size => "size",
                    BindingType::Duration => "duration",
                    BindingType::Text => "text",
                    BindingType::Path => "path",
                }
        {
            return Err("Conflicting DSL placeholder type.".into());
        }
        references[index] += 1;
        output.push_str(&match &slot.marker {
            Value::Text(t) => format!("\"{t}\""),
            Value::Number(n) => format!(
                "{n}{}",
                if slot.kind == BindingType::Duration {
                    "s"
                } else {
                    "b"
                }
            ),
        });
    }
    let expr = Query::parse(&output)
        .map_err(|e| format!("Invalid bound recipe DSL: {}", e.message))?
        .expression()
        .clone();
    let mut found = vec![0usize; slots.len()];
    validate_slots(&expr, slots, &mut found)?;
    if found != references || found.contains(&0) {
        return Err(
            "Bindings must be used as compatible complete values, with no marker collisions."
                .into(),
        );
    }
    Ok(expr)
}
fn validate_slots(expr: &Expr, slots: &[Slot], found: &mut [usize]) -> Result<(), String> {
    match expr {
        Expr::And(a, b) | Expr::Or(a, b) => {
            validate_slots(a, slots, found)?;
            validate_slots(b, slots, found)?;
        }
        Expr::Not(inner) => validate_slots(inner, slots, found)?,
        Expr::Compare { field, value, .. } => check_slot(*field, value, slots, found)?,
        Expr::In { field, values, .. } => {
            for value in values {
                check_slot(*field, value, slots, found)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn check_slot(
    field: Field,
    value: &Value,
    slots: &[Slot],
    found: &mut [usize],
) -> Result<(), String> {
    for (i, slot) in slots.iter().enumerate() {
        if value == &slot.marker {
            let compatible = match slot.kind {
                BindingType::Size => matches!(field, Field::Size | Field::Apparent | Field::Wasted),
                BindingType::Duration => field == Field::Age,
                BindingType::Text => matches!(field, Field::Name | Field::Ext | Field::Owner),
                BindingType::Path => field == Field::Path,
            };
            if !compatible {
                return Err(format!(
                    "Binding {} is incompatible with field {}",
                    slot.name,
                    field.name()
                ));
            }
            found[i] += 1;
        }
    }
    Ok(())
}
fn bind_value(
    kind: BindingType,
    text: &str,
    context: SearchContext,
    resolver: &dyn Fn([i32; 3]) -> Option<u64>,
) -> Result<Value, String> {
    if text.is_empty() {
        return Err("empty values are unsupported".into());
    }
    if matches!(kind, BindingType::Text | BindingType::Path) {
        let text = if text.starts_with('"') || text.starts_with('\'') {
            let quote = text.chars().next().unwrap();
            if text.len() < 2 || !text.ends_with(quote) {
                return Err("unclosed literal quote".into());
            }
            &text[1..text.len() - 1]
        } else {
            text
        };
        if text.is_empty() {
            return Err("empty literal values are unsupported".into());
        }
        if text.contains('"') && text.contains('\'') {
            return Err(
                "literal contains both quote styles and cannot round-trip through editable DSL"
                    .into(),
            );
        }
        return Ok(Value::Text(text.into()));
    }
    let (prefix, field, op) = if kind == BindingType::Size {
        ("files exactly", Field::Size, Op::Eq)
    } else {
        ("files older than", Field::Age, Op::Gt)
    };
    let translated = translate_with_resolver(
        &format!("{prefix} {text}"),
        context,
        SearchMode::Natural,
        resolver,
    );
    let Some(query) = translated.query else {
        return Err("invalid typed quantity".into());
    };
    if let Expr::And(flag, comparison) = query.expression()
        && matches!(**flag, Expr::Flag(Flag::File))
        && let Expr::Compare {
            field: actual,
            op: actual_op,
            value: Value::Number(n),
        } = &**comparison
        && *actual == field
        && *actual_op == op
    {
        return Ok(Value::Number(*n));
    }
    Err("capture must contain exactly one typed quantity, without extra conditions".into())
}
fn replace(expr: &mut Expr, slots: &[Slot], values: &[Value]) {
    match expr {
        Expr::And(a, b) | Expr::Or(a, b) => {
            replace(a, slots, values);
            replace(b, slots, values)
        }
        Expr::Not(inner) => replace(inner, slots, values),
        Expr::Compare { value, .. } => replace_value(value, slots, values),
        Expr::In { values: list, .. } => {
            for value in list {
                replace_value(value, slots, values)
            }
        }
        _ => {}
    }
}
fn replace_value(value: &mut Value, slots: &[Slot], values: &[Value]) {
    if let Some(index) = slots.iter().position(|s| s.marker == *value) {
        *value = values[index].clone();
    }
}
fn quote(text: &str) -> Result<String, String> {
    if !text.contains('"') {
        Ok(format!("\"{text}\""))
    } else if !text.contains('\'') {
        Ok(format!("'{text}'"))
    } else {
        Err(
            "Literal containing both quote styles cannot be represented by the editable DSL."
                .into(),
        )
    }
}
fn render(expr: &Expr, parent: u8) -> Result<String, String> {
    let precedence = match expr {
        Expr::Or(..) => 1,
        Expr::And(..) => 2,
        Expr::Not(_) => 3,
        _ => 4,
    };
    let text = match expr {
        Expr::And(a, b) => format!("{} and {}", render(a, precedence)?, render(b, precedence)?),
        Expr::Or(a, b) => format!("{} or {}", render(a, precedence)?, render(b, precedence)?),
        Expr::Not(inner) => format!("not ({})", render(inner, 0)?),
        Expr::Flag(flag) => flag.name().into(),
        Expr::NameContains(text) => quote(text)?,
        Expr::Compare { field, op, value } => format!(
            "{} {} {}",
            field.name(),
            match op {
                Op::Lt => "<",
                Op::Le => "<=",
                Op::Gt => ">",
                Op::Ge => ">=",
                Op::Eq => "=",
                Op::Ne => "!=",
                Op::Contains => "~",
                Op::NotContains => "!~",
                Op::StartsWith => "startswith",
                Op::EndsWith => "endswith",
            },
            render_value(*field, value)?
        ),
        Expr::In {
            field,
            values,
            negated,
        } => format!(
            "{} {}in ({})",
            field.name(),
            if *negated { "not " } else { "" },
            values
                .iter()
                .map(|v| render_value(*field, v))
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        ),
    };
    Ok(if precedence < parent {
        format!("({text})")
    } else {
        text
    })
}
fn render_value(field: Field, value: &Value) -> Result<String, String> {
    match value {
        Value::Text(t) => quote(t),
        Value::Number(n) => Ok(match field {
            Field::Size | Field::Apparent | Field::Wasted => format!("{n}b"),
            Field::Age => format!("{n}s"),
            Field::Modified | Field::Created => format!("@{n}"),
            _ => n.to_string(),
        }),
    }
}
pub(super) fn failure(_input: &str, span: Range<usize>, message: String) -> Translation {
    Translation {query:None,dsl:None,entities:vec![],diagnostics:vec![Diagnostic {span,message,hint:"Use one unambiguous recipe with valid typed values, or choose explicit DSL/Name mode.".into()}],mode:SearchMode::Natural}
}
