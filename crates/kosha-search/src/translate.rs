// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use crate::calendar::{self, civil_days, date_for_days};
use crate::numbers::{self, Quantity};
use kosha_query::{Expr, Field, Op, Query, Span, Value};

/// Caller-supplied clock and local calendar. Offsets are fixed for this
/// translation; callers must retranslate when their calendar/offset changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchContext {
    pub now: u32,
    pub local_date: [i32; 3],
    pub utc_offset_seconds: i32,
}

impl SearchContext {
    pub const fn new(now: u32, local_date: [i32; 3]) -> Self {
        Self {
            now,
            local_date,
            utc_offset_seconds: 0,
        }
    }
    pub const fn with_utc_offset(mut self, seconds: i32) -> Self {
        self.utc_offset_seconds = seconds;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchMode {
    Natural,
    Dsl,
    Literal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntityKind {
    Recipe,
    Flag,
    Age,
    Exclusion,
    Connector,
    Kind,
    FileType,
    Size,
    Count,
    Name,
    Path,
    Modified,
    Created,
    Literal,
    Dsl,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entity {
    pub kind: EntityKind,
    pub span: Span,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub span: Span,
    pub message: String,
    pub hint: String,
}

#[derive(Clone, Debug)]
pub struct Translation {
    /// Displayable/editable canonical DSL. Absent if any input was unresolved.
    pub dsl: Option<String>,
    pub query: Option<Query>,
    pub entities: Vec<Entity>,
    pub diagnostics: Vec<Diagnostic>,
    pub mode: SearchMode,
}

impl Translation {
    pub fn is_valid(&self) -> bool {
        self.query.is_some() && self.diagnostics.is_empty()
    }
}

/// Automatic routing preserves intentional DSL; interfaces with an explicit
/// mode toggle should call [`translate_with_mode`] instead.
pub fn translate(input: &str, context: SearchContext) -> Translation {
    let mode = if looks_like_dsl(input) {
        SearchMode::Dsl
    } else {
        SearchMode::Natural
    };
    translate_with_mode(input, context, mode)
}

pub fn translate_with_mode(input: &str, context: SearchContext, mode: SearchMode) -> Translation {
    translate_with_resolver(input, context, mode, |date| {
        let seconds = civil_days(date)?
            .checked_mul(86400)?
            .checked_sub(i64::from(context.utc_offset_seconds))?;
        u64::try_from(seconds).ok()
    })
}

/// Supply local midnight for each boundary individually, including historical
/// daylight-saving offsets. The callback is caller-owned; this crate reads no
/// ambient timezone or clock. Return `None` for an unresolvable date.
pub fn translate_with_resolver(
    input: &str,
    context: SearchContext,
    mode: SearchMode,
    resolve_date: impl Fn([i32; 3]) -> Option<u64>,
) -> Translation {
    let mut out = Translation {
        dsl: None,
        query: None,
        entities: vec![],
        diagnostics: vec![],
        mode,
    };
    if input.trim().is_empty() {
        return out;
    }
    if input.len() > 16384 {
        out.diagnostics.push(Diagnostic {
            span: 0..input.len(),
            message: "Search is too long.".into(),
            hint: "Use at most 16 KiB of search text.".into(),
        });
        return out;
    }
    if mode == SearchMode::Dsl {
        match Query::parse(input) {
            Ok(query) => {
                out.entities.push(Entity {
                    kind: EntityKind::Dsl,
                    span: 0..input.len(),
                    value: input.into(),
                });
                out.dsl = Some(input.into());
                out.query = Some(query);
            }
            Err(e) => out.diagnostics.push(Diagnostic {
                span: e.span,
                message: e.message,
                hint: e.hint.unwrap_or_else(|| {
                    "Edit the query syntax, or switch to natural search.".into()
                }),
            }),
        }
        return out;
    }
    if mode == SearchMode::Literal {
        literal(input, &mut out);
        return out;
    }
    if let Some((phrase, dsl)) = crate::recipes::builtin(input) {
        let result = translate_with_resolver(dsl, context, SearchMode::Dsl, resolve_date);
        return crate::recipes::as_recipe(result, input, phrase);
    }
    let tokens = match tokenize(input) {
        Ok(tokens) => tokens,
        Err(diagnostic) => {
            out.diagnostics.push(diagnostic);
            return out;
        }
    };
    if tokens.len() > 256 {
        out.diagnostics.push(Diagnostic {
            span: 0..input.len(),
            message: "Too many search terms.".into(),
            hint: "Use at most 256 terms.".into(),
        });
        return out;
    }
    if tokens.len() == 1 && tokens[0].quoted {
        literal(&tokens[0].raw, &mut out);
        if let Some(entity) = out.entities.first_mut() {
            entity.span = 0..input.len();
        }
        return out;
    }
    let trailing_incomplete = tokens
        .last()
        .filter(|t| {
            !t.quoted
                && matches!(
                    t.lower.as_str(),
                    "and" | "or" | "with" | "that" | "which" | "are" | "of" | "the" | "a" | "an"
                )
        })
        .map(|t| t.span.clone());
    let mut parser = Natural {
        tokens,
        at: 0,
        clauses: vec![],
        extensions: vec![],
        kind: None,
        entities: vec![],
        diagnostics: vec![],
        context,
        structured: false,
        resolve_date: &resolve_date,
        implicit_files: false,
        in_exclusion: false,
        exclusion_list: false,
    };
    while parser.at < parser.tokens.len() {
        if parser.consume() {
            continue;
        }
        let token = &parser.tokens[parser.at];
        parser.diagnostics.push(Diagnostic { span: token.span.clone(),
            message: format!("I could not interpret ‘{}’.", token.raw),
            hint: "Use named \"text\" for a filename, in \"path\" for a path, or quote the whole search literally.".into() });
        parser.at += 1;
    }
    if !parser.structured {
        literal(input, &mut out);
        return out;
    }
    if let Some(last_end) = parser
        .entities
        .iter()
        .filter(|e| !matches!(e.kind, EntityKind::Connector | EntityKind::Exclusion))
        .map(|e| e.span.end)
        .max()
        && let Some(token) = parser.tokens.iter().find(|t| {
            t.span.start >= last_end
                && !t.quoted
                && matches!(
                    t.lower.as_str(),
                    "and"
                        | "with"
                        | "whose"
                        | "that"
                        | "which"
                        | "are"
                        | "is"
                        | "have"
                        | "has"
                        | "of"
                )
        })
    {
        parser.diagnostics.push(Diagnostic {
            span: token.span.clone(),
            message: "Incomplete search condition after this connector.".into(),
            hint: "Add the requested condition, or remove this connector.".into(),
        });
    }
    if let Some(span) = trailing_incomplete {
        parser.diagnostics.push(Diagnostic {
            span,
            message: "Incomplete search condition.".into(),
            hint: "Finish the condition after this word, or remove it.".into(),
        });
    }
    out.entities = parser.entities;
    out.entities.sort_by_key(|e| e.span.start);
    out.diagnostics = parser.diagnostics;
    if parser.clauses.len() > 128 {
        out.diagnostics.push(Diagnostic {
            span: 0..input.len(),
            message: "Too many search conditions.".into(),
            hint: "Use at most 128 predicates in one natural-language search.".into(),
        });
    }
    if !out.diagnostics.is_empty() {
        return out;
    }
    let mut clauses = vec![];
    if let Some(kind) = parser.kind {
        clauses.push(Clause::parsed(&if kind.contains(" or ") {
            format!("({kind})")
        } else {
            kind.into()
        }));
    } else if !parser.extensions.is_empty() || parser.implicit_files {
        clauses.push(Clause::parsed("file"));
    }
    if !parser.extensions.is_empty() {
        let extensions: Vec<_> = parser.extensions.into_iter().fold(vec![], |mut acc, ext| {
            if !acc.contains(&ext) {
                acc.push(ext);
            }
            acc
        });
        let dsl = if extensions.len() == 1 {
            format!("ext = {}", extensions[0])
        } else {
            format!("ext in ({})", extensions.join(", "))
        };
        clauses.push(Clause::parsed(&dsl));
    }
    clauses.extend(parser.clauses);
    if clauses.is_empty() {
        literal(input, &mut out);
        return out;
    }
    let dsl = clauses
        .iter()
        .map(|c| c.dsl.as_str())
        .collect::<Vec<_>>()
        .join(" and ");
    let expression = clauses
        .into_iter()
        .map(|c| c.expr)
        .reduce(|a, b| Expr::And(Box::new(a), Box::new(b)))
        .expect("clauses");
    out.query = Some(Query::from_expression(expression, dsl.clone()));
    out.dsl = Some(dsl);
    out
}

fn looks_like_dsl(input: &str) -> bool {
    let first = input
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let field = kosha_query::field_for_word(&first).is_some();
    let flag = kosha_query::flag_for_word(&first).is_some();
    let lower = input.to_ascii_lowercase();
    let rest = lower
        .trim_start()
        .get(first.len()..)
        .unwrap_or("")
        .trim_start();
    let symbols = input.chars().any(|c| "=<>~!".contains(c));
    let word_operator = lower
        .split_whitespace()
        .any(|word| matches!(word, "startswith" | "endswith"));
    let membership = rest
        .strip_prefix("not ")
        .unwrap_or(rest)
        .trim_start()
        .strip_prefix("in")
        .is_some_and(|tail| tail.trim_start().starts_with('('));
    let negative_subject = rest.split_whitespace().next().unwrap_or("");
    let negative_dsl = first == "not"
        && (kosha_query::flag_for_word(negative_subject).is_some()
            || negative_subject == "not"
            || rest.starts_with('(')
            || (kosha_query::field_for_word(negative_subject).is_some()
                && (symbols || word_operator || lower.contains('('))));
    ((field || flag) && (symbols || word_operator))
        || (field && membership)
        || negative_dsl
        || input.contains("&&")
        || input.contains("||")
        || input.trim_start().starts_with('(')
}

fn quoted(value: &str) -> Option<String> {
    if !value.contains('"') {
        Some(format!("\"{value}\""))
    } else if !value.contains('\'') {
        Some(format!("'{value}'"))
    } else {
        None
    }
}

fn literal(value: &str, out: &mut Translation) {
    if let Some(text) = quoted(value) {
        let dsl = format!("name ~ {text}");
        out.query = Some(Query::from_expression(
            Expr::NameContains(value.into()),
            dsl.clone(),
        ));
        out.dsl = Some(dsl);
        out.entities.push(Entity {
            kind: EntityKind::Literal,
            span: 0..value.len(),
            value: value.into(),
        });
    } else {
        out.diagnostics.push(Diagnostic {
            span: 0..value.len(),
            message: "This text contains both quote styles.".into(),
            hint: "Search for a shorter literal containing only one quote style.".into(),
        });
    }
}

struct Clause {
    dsl: String,
    expr: Expr,
}
impl Clause {
    fn parsed(dsl: &str) -> Self {
        Self {
            dsl: dsl.into(),
            expr: Query::parse(dsl)
                .expect("translator emits valid non-date DSL")
                .expression()
                .clone(),
        }
    }
}

fn combine_clauses(clauses: Vec<Clause>) -> Clause {
    let dsl = clauses
        .iter()
        .map(|c| c.dsl.as_str())
        .collect::<Vec<_>>()
        .join(" and ");
    let expr = clauses
        .into_iter()
        .map(|c| c.expr)
        .reduce(|a, b| Expr::And(Box::new(a), Box::new(b)))
        .expect("nonempty condition");
    Clause { dsl, expr }
}

fn extension_clause(values: &[String]) -> Clause {
    let mut unique = vec![];
    for value in values {
        if !unique.contains(value) {
            unique.push(value.clone());
        }
    }
    if unique.len() == 1 {
        Clause::parsed(&format!("ext = {}", unique[0]))
    } else {
        Clause::parsed(&format!("ext in ({})", unique.join(", ")))
    }
}

struct Token {
    raw: String,
    lower: String,
    span: Span,
    quoted: bool,
}
fn tokenize(input: &str) -> Result<Vec<Token>, Diagnostic> {
    let mut tokens = vec![];
    let mut it = input.char_indices().peekable();
    while let Some((start, c)) = it.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == ',' {
            tokens.push(Token {
                raw: ",".into(),
                lower: ",".into(),
                span: start..start + 1,
                quoted: false,
            });
            continue;
        }
        let is_quoted = c == '"' || c == '\'';
        let mut end = start + c.len_utf8();
        if is_quoted {
            let mut closed = false;
            for (i, next) in it.by_ref() {
                end = i + next.len_utf8();
                if next == c {
                    closed = true;
                    break;
                }
            }
            if !closed {
                return Err(Diagnostic {
                    span: start..input.len(),
                    message: "Unclosed quote.".into(),
                    hint: "Add the closing quote.".into(),
                });
            }
        } else {
            while let Some(&(i, next)) = it.peek() {
                if next.is_whitespace() || next == ',' {
                    break;
                }
                end = i + next.len_utf8();
                it.next();
            }
        }
        let raw = if is_quoted {
            &input[start + c.len_utf8()..end - c.len_utf8()]
        } else {
            &input[start..end]
        };
        tokens.push(Token {
            raw: raw.into(),
            lower: raw.to_ascii_lowercase(),
            span: start..end,
            quoted: is_quoted,
        });
    }
    Ok(tokens)
}

struct Natural<'a> {
    tokens: Vec<Token>,
    at: usize,
    clauses: Vec<Clause>,
    extensions: Vec<String>,
    kind: Option<&'static str>,
    entities: Vec<Entity>,
    diagnostics: Vec<Diagnostic>,
    context: SearchContext,
    structured: bool,
    resolve_date: &'a dyn Fn([i32; 3]) -> Option<u64>,
    implicit_files: bool,
    in_exclusion: bool,
    exclusion_list: bool,
}

impl Natural<'_> {
    fn word(&self, offset: usize) -> &str {
        self.tokens
            .get(self.at + offset)
            .filter(|t| !t.quoted)
            .map_or("", |t| t.lower.as_str())
    }
    fn phrase(&self, words: &[&str]) -> bool {
        words.iter().enumerate().all(|(i, w)| {
            self.tokens
                .get(self.at + i)
                .is_some_and(|t| !t.quoted && t.lower == *w)
        })
    }
    fn record(&mut self, kind: EntityKind, count: usize, value: String) {
        let end = self.at + count;
        self.entities.push(Entity {
            kind,
            span: self.tokens[self.at].span.start..self.tokens[end - 1].span.end,
            value,
        });
        self.at = end;
        self.structured = true;
    }
    fn consume(&mut self) -> bool {
        if self.tokens[self.at].quoted {
            return false;
        }
        let word = self.word(0).to_string();
        if word == "and" && matches!(self.word(1), "and" | "or") {
            self.incomplete("Repeated or conflicting connectors.");
            self.at += 1;
            return true;
        }
        if matches!(word.as_str(), "not" | "except" | "excluding" | "without")
            || self.phrase(&["other", "than"])
            || self.phrase(&["but", "not"])
        {
            let count = if matches!(word.as_str(), "other" | "but") {
                2
            } else {
                1
            };
            let list = matches!(word.as_str(), "except" | "excluding" | "without" | "other");
            self.record(EntityKind::Exclusion, count, "not".into());
            if self.at >= self.tokens.len() {
                self.incomplete("Incomplete exclusion.");
                return true;
            }
            if matches!(
                self.word(0),
                "files"
                    | "file"
                    | "folders"
                    | "folder"
                    | "directories"
                    | "directory"
                    | "dir"
                    | "dirs"
            ) {
                self.incomplete("Excluding an entry kind here is ambiguous.");
                return false;
            }
            let clause_start = self.clauses.len();
            let ext_start = self.extensions.len();
            let old_kind = self.kind;
            self.in_exclusion = true;
            self.exclusion_list = list;
            let consumed = self.consume_atom();
            self.in_exclusion = false;
            self.exclusion_list = false;
            if !consumed {
                self.incomplete("Incomplete or unsupported exclusion.");
                return false;
            }
            let mut clauses: Vec<_> = self.clauses.drain(clause_start..).collect();
            let extensions: Vec<_> = self.extensions.drain(ext_start..).collect();
            if !extensions.is_empty() {
                self.implicit_files = true;
                clauses.insert(0, extension_clause(&extensions));
            }
            if self.kind != old_kind {
                self.kind = old_kind;
                self.incomplete("Excluding an entry kind here is ambiguous.");
            }
            if !clauses.is_empty() {
                let inner = combine_clauses(clauses);
                self.clauses.push(Clause {
                    dsl: format!("not ({})", inner.dsl),
                    expr: Expr::Not(Box::new(inner.expr)),
                });
            } else {
                self.incomplete("An exclusion must contain a complete condition.");
            }
            return true;
        }
        if word == "or" {
            self.incomplete(
                "Ambiguous or: use an explicit file-type list or grouped query syntax.",
            );
            self.at += 1;
            return true;
        }
        if self.phrase(&["whose"]) {
            self.at += 1;
            return true;
        }
        self.consume_atom()
    }
    fn incomplete(&mut self, message: &str) {
        let span = self.tokens.get(self.at).map_or_else(
            || self.tokens.last().map_or(0..0, |t| t.span.clone()),
            |t| t.span.clone(),
        );
        self.diagnostics.push(Diagnostic {
            span,
            message: message.into(),
            hint: "Finish this condition, or use explicit grouped DSL in query mode.".into(),
        });
        self.structured = true;
    }
    fn consume_atom(&mut self) -> bool {
        let word = self.word(0).to_string();
        if self.tokens[self.at].quoted {
            return false;
        }
        if matches!(
            word.as_str(),
            "show"
                | "find"
                | "list"
                | "search"
                | "me"
                | "all"
                | "the"
                | "a"
                | "an"
                | "with"
                | "that"
                | "which"
                | "are"
                | "and"
                | "of"
                | "is"
                | "have"
                | "has"
                | "please"
                | "only"
        ) {
            self.at += 1;
            return true;
        }
        let kind = match word.as_str() {
            "file" | "files" => Some("file"),
            "folder" | "folders" | "directory" | "directories" | "dir" | "dirs" => Some("dir"),
            _ => None,
        };
        if let Some(kind) = kind {
            let next_kind = match self.word(2) {
                "file" | "files" => Some("file"),
                "folder" | "folders" | "directory" | "directories" | "dir" | "dirs" => Some("dir"),
                _ => None,
            };
            if self.word(1) == "or"
                && self.tokens.get(self.at + 1).is_some_and(|t| !t.quoted)
                && self.tokens.get(self.at + 2).is_some_and(|t| !t.quoted)
                && let Some(next) = next_kind
            {
                if self.kind.is_some() {
                    self.incomplete("Repeated entry-kind alternatives are ambiguous.");
                }
                self.kind = Some(if kind == next { kind } else { "file or dir" });
                self.record(EntityKind::Kind, 1, kind.into());
                self.record(EntityKind::Connector, 1, "or".into());
                self.record(EntityKind::Kind, 1, next.into());
                return true;
            }
            if self.kind.is_some_and(|old| old != kind) {
                self.diagnostics.push(Diagnostic {
                    span: self.tokens[self.at].span.clone(),
                    message: "Files and folders are conflicting kinds here.".into(),
                    hint: "Choose one kind, or use file or dir in query mode.".into(),
                });
            }
            self.kind = Some(kind);
            self.record(EntityKind::Kind, 1, kind.into());
            return true;
        }
        if self.text_condition() {
            return true;
        }
        if self.type_list() {
            return true;
        }
        if self.flag() {
            return true;
        }
        if self.age() {
            return true;
        }
        if matches!(
            word.as_str(),
            "modified" | "changed" | "updated" | "created" | "creation" | "born"
        ) {
            return self.date();
        }
        if self.qualified_size() {
            return true;
        }
        if self.phrase(&["between"]) {
            if let Some((first, second, used)) = self.size_range_at(self.at) {
                if size_in_bytes(&first) > size_in_bytes(&second) {
                    self.incomplete("The start size is greater than the end size.");
                }
                self.clauses.push(Clause::parsed(&format!(
                    "size >= {first} and size <= {second}"
                )));
                self.record(EntityKind::Size, used, format!("{first}..{second}"));
                return true;
            }
            self.structured = true;
        }
        if let Some((op, count)) = self.comparator() {
            if let Some(quantity) = self.quantity_at(self.at + count)
                && let Some(amount) = quantity.integer()
            {
                let unit = self.word(count + quantity.used);
                if matches!(unit, "files" | "items" | "children") {
                    let field = if unit == "children" {
                        "children"
                    } else {
                        "items"
                    };
                    if self.kind == Some("file") {
                        self.incomplete(
                            "A file-count condition requires folders, not a files subject.",
                        );
                    } else if self.kind.is_none() {
                        self.kind = Some("dir");
                    }
                    self.clauses
                        .push(Clause::parsed(&format!("{field} {op} {amount}")));
                    self.record(
                        EntityKind::Count,
                        count + quantity.used + 1,
                        format!("{field} {op} {amount}"),
                    );
                    return true;
                }
            }
            if let Some((size, used)) = self.size_at(self.at + count) {
                self.clauses
                    .push(Clause::parsed(&format!("size {op} {size}")));
                self.record(EntityKind::Size, count + used, format!("{op} {size}"));
                return true;
            }
            self.structured = true;
        }
        if word == "size"
            && self.tokens.get(self.at + 1).is_some_and(|t| {
                matches!(
                    t.lower.as_str(),
                    "over"
                        | "above"
                        | "under"
                        | "below"
                        | ">"
                        | "<"
                        | ">="
                        | "<="
                        | "="
                        | "greater"
                        | "larger"
                        | "bigger"
                        | "smaller"
                        | "less"
                        | "at"
                        | "between"
                        | "exactly"
                )
            })
        {
            self.structured = true;
            self.at += 1;
            return true;
        }
        false
    }
    fn text_condition(&mut self) -> bool {
        let first = self.word(0).to_string();
        let (field, name, kind, mut offset) = match first.as_str() {
            "name" | "names" | "filename" | "filenames" => {
                (Field::Name, "name", EntityKind::Name, 1)
            }
            "path" | "paths" => (Field::Path, "path", EntityKind::Path, 1),
            "in" | "under"
                if self.size_at(self.at + 1).is_none()
                    && !self
                        .word(1)
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_digit()) =>
            {
                (Field::Path, "path", EntityKind::Path, 0)
            }
            "named" | "containing" | "contains" | "starting" | "starts" | "beginning"
            | "begins" | "ending" | "ends" => (Field::Name, "name", EntityKind::Name, 0),
            _ => return false,
        };
        let marker = if self.tokens.get(self.at + offset).is_some_and(|t| t.quoted) {
            String::new()
        } else {
            self.word(offset).to_string()
        };
        let mut negated = false;
        let (op, spelling) = if matches!(marker.as_str(), "does" | "do")
            && self.word(offset + 1) == "not"
            && matches!(self.word(offset + 2), "contain" | "contains")
        {
            negated = true;
            offset += 3;
            (Op::Contains, "~")
        } else if matches!(
            marker.as_str(),
            "starting" | "starts" | "start" | "beginning" | "begins" | "begin"
        ) && self.word(offset + 1) == "with"
        {
            offset += 2;
            (Op::StartsWith, "startswith")
        } else if matches!(marker.as_str(), "ending" | "ends" | "end")
            && self.word(offset + 1) == "with"
        {
            let token = self.tokens.get(self.at + offset + 2);
            if offset == 0
                && token
                    .is_some_and(|t| !t.quoted && !t.lower.trim_start_matches('.').contains('.'))
            {
                return false;
            }
            offset += 2;
            (Op::EndsWith, "endswith")
        } else if matches!(
            marker.as_str(),
            "contains" | "containing" | "contain" | "in" | "under"
        ) {
            offset += 1;
            (Op::Contains, "~")
        } else if matches!(marker.as_str(), "named" | "is" | "equals" | "equal") {
            offset += 1;
            if marker == "equal" && self.word(offset) == "to" {
                offset += 1;
            }
            (Op::Eq, "=")
        } else if offset == 1 {
            (Op::Eq, "=")
        } else {
            return false;
        };
        let Some(token) = self.tokens.get(self.at + offset) else {
            self.incomplete("Incomplete name/path condition.");
            return false;
        };
        if !token.quoted
            && matches!(
                token.lower.as_str(),
                "and" | "or" | "not" | "with" | "except"
            )
        {
            self.incomplete("Quote operator words used as literal text.");
            return false;
        }
        let value = token.raw.clone();
        let Some(text) = quoted(&value) else {
            self.incomplete("This literal contains both quote styles.");
            return false;
        };
        let expr = Expr::Compare {
            field,
            op,
            value: Value::Text(value.clone()),
        };
        let dsl = format!("{name} {spelling} {text}");
        self.clauses.push(if negated {
            Clause {
                dsl: format!("not ({dsl})"),
                expr: Expr::Not(Box::new(expr)),
            }
        } else {
            Clause { dsl, expr }
        });
        self.record(
            kind,
            offset + 1,
            if negated {
                format!("not {value}")
            } else {
                value
            },
        );
        true
    }
    fn type_atom_at(&self, index: usize, generic: bool) -> Option<(Vec<String>, usize)> {
        let token = self.tokens.get(index)?;
        if generic {
            let raw = token.lower.strip_prefix('.').unwrap_or(&token.lower);
            if raw.is_empty()
                || !raw
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                || (!token.quoted
                    && matches!(
                        raw,
                        "and"
                            | "or"
                            | "not"
                            | "except"
                            | "excluding"
                            | "without"
                            | "with"
                            | "in"
                            | "over"
                            | "under"
                            | "modified"
                            | "created"
                            | "files"
                            | "folders"
                    ))
            {
                return None;
            }
            return Some((vec![raw.into()], 1));
        }
        if token.quoted {
            return None;
        }
        let next = self
            .tokens
            .get(index + 1)
            .filter(|t| !t.quoted)
            .map_or("", |t| t.lower.as_str());
        if token.lower == "source" && next == "code" {
            return Some((extensions("code")?.iter().map(|s| (*s).into()).collect(), 2));
        }
        if matches!(
            token.lower.as_str(),
            "c" | "c++" | "cpp" | "rust" | "python" | "java" | "javascript" | "typescript" | "go"
        ) && matches!(next, "code" | "source")
        {
            let values = extensions(&token.lower)?;
            return Some((values.iter().map(|s| (*s).into()).collect(), 2));
        }
        extensions(&token.lower).map(|values| (values.iter().map(|s| (*s).into()).collect(), 1))
    }
    fn type_list(&mut self) -> bool {
        let word = self.word(0).to_string();
        let explicit = matches!(
            word.as_str(),
            "extension" | "extensions" | "ext" | "types" | "type" | "formats" | "format"
        );
        let ending = matches!(word.as_str(), "ending" | "ends") && self.word(1) == "with";
        // Quoted suffixes and multi-part suffixes describe the name, not an
        // extension. Leave them for the name/path parser.
        if ending
            && self
                .tokens
                .get(self.at + 2)
                .is_some_and(|t| t.quoted || t.lower.trim_start_matches('.').contains('.'))
        {
            return false;
        }
        let generic =
            explicit && !matches!(word.as_str(), "types" | "type" | "formats" | "format") || ending;
        let prefix = if ending { 2 } else { usize::from(explicit) };
        let Some((values, used)) = self.type_atom_at(self.at + prefix, generic) else {
            if explicit || ending {
                self.incomplete("Missing or invalid extension/type list.");
            }
            return false;
        };
        if !self.extensions.is_empty() && !self.in_exclusion {
            self.incomplete(
                "Separate file-type filters are ambiguous; use one explicit alternatives list.",
            );
        }
        let explicit_list = explicit || self.exclusion_list;
        let negative_ambiguous = self.in_exclusion && !self.exclusion_list && !explicit;
        let mut atom_values = values;
        self.extensions.extend(atom_values.iter().cloned());
        self.record(EntityKind::FileType, prefix + used, atom_values.join(","));
        // Specific product-family qualifiers preserve their existing entities.
        if matches!(word.as_str(), "word" | "excel")
            && self.tokens.get(self.at).is_some_and(|t| !t.quoted)
            && matches!(
                self.word(0),
                "doc"
                    | "document"
                    | "documents"
                    | "sheet"
                    | "sheets"
                    | "spreadsheet"
                    | "spreadsheets"
            )
        {
            self.record(EntityKind::FileType, 1, atom_values.join(","));
        }
        let mut alternatives = false;
        loop {
            if self.tokens.get(self.at).is_some_and(|t| t.quoted) {
                break;
            }
            let connector = self.word(0);
            if !matches!(connector, "or" | "and" | ",") {
                break;
            }
            let Some((values, used)) = self.type_atom_at(self.at + 1, generic) else {
                break;
            };
            if connector == "and" && !explicit_list {
                self.incomplete(
                    "Ambiguous and between file types: use or, or an explicit types list.",
                );
                break;
            }
            if negative_ambiguous {
                self.incomplete(
                    "Ambiguous exclusion alternatives: use except or an explicit extensions list.",
                );
                break;
            }
            let spelling = connector.to_string();
            self.record(
                EntityKind::Connector,
                1,
                if spelling == "," {
                    "or".into()
                } else {
                    spelling
                },
            );
            atom_values = values;
            self.extensions.extend(atom_values.iter().cloned());
            self.record(EntityKind::FileType, used, atom_values.join(","));
            alternatives = true;
        }
        if alternatives
            && self.kind.is_none()
            && self.at < self.tokens.len()
            && !matches!(self.word(0), "file" | "files")
            && !self.in_exclusion
        {
            self.incomplete(
                "Type alternatives with trailing conditions need an explicit files subject.",
            );
        }
        true
    }
    fn flag(&mut self) -> bool {
        let word = self.word(0).to_string();
        let flag = match word.as_str() {
            "empty" => Some("empty"),
            "sparse" => Some("sparse"),
            "unreadable" | "inaccessible" => Some("unreadable"),
            "partial" => Some("partial"),
            "hardlinked" | "hardlink" => Some("hardlink"),
            "link" | "links" | "symlink" | "symlinks" => Some("link"),
            _ => None,
        };
        let Some(flag) = flag else {
            return false;
        };
        let following_union = matches!(self.word(1), "file" | "files" | "folder" | "folders")
            && self.word(2) == "or"
            && matches!(self.word(3), "file" | "files" | "folder" | "folders");
        if flag == "empty" && (self.kind == Some("file or dir") || following_union) {
            self.clauses.push(Clause::parsed(
                "((file and apparent = 0b) or (dir and empty))",
            ));
            self.record(EntityKind::Flag, 1, "empty files or folders".into());
            return true;
        }
        if flag == "empty"
            && (self.kind == Some("file")
                || matches!(self.word(1), "file" | "files")
                || (self.kind.is_none()
                    && (!self.extensions.is_empty()
                        || self.type_atom_at(self.at + 1, false).is_some())))
        {
            self.clauses.push(Clause::parsed("apparent = 0b"));
            self.record(EntityKind::Flag, 1, "apparent = 0b".into());
            return true;
        }
        self.clauses.push(Clause::parsed(flag));
        self.record(EntityKind::Flag, 1, flag.into());
        true
    }
    fn duration_at(&self, index: usize) -> Option<(u64, usize)> {
        let (amount, unit, used) = self.quantity_unit_at(index)?;
        let scale = match unit {
            "s" | "second" | "seconds" => 1,
            "min" | "minute" | "minutes" => 60,
            "h" | "hr" | "hour" | "hours" => 3600,
            "d" | "day" | "days" => 86400,
            "w" | "week" | "weeks" => 604800,
            _ => return None,
        };
        Some((amount.scaled_integer(scale)?, used))
    }
    fn age(&mut self) -> bool {
        let first = self.word(0);
        let prefix = if first == "age" { 1 } else { 0 };
        let comparison = if first == "older" && self.word(1) == "than" {
            Some((">", 2))
        } else if matches!(first, "newer" | "younger") && self.word(1) == "than" {
            Some(("<", 2))
        } else if prefix == 1 {
            self.comparator_at(self.at + 1)
                .map(|(op, count)| (op, count + 1))
        } else {
            None
        };
        let Some((op, count)) = comparison else {
            return false;
        };
        let Some((seconds, used)) = self.duration_at(self.at + count) else {
            self.incomplete("Missing or invalid age duration; use explicit seconds, minutes, hours, days, or weeks.");
            return false;
        };
        self.clauses
            .push(Clause::parsed(&format!("age {op} {seconds}s")));
        self.record(EntityKind::Age, count + used, format!("{op} {seconds}s"));
        true
    }
    fn comparator_at(&self, index: usize) -> Option<(&'static str, usize)> {
        if self.tokens.get(index)?.quoted {
            return None;
        }
        for (words, op) in [
            (&["more", "than"][..], ">"),
            (&["no", "more", "than"][..], "<="),
            (&["no", "less", "than"][..], ">="),
            (&["greater", "than", "or", "equal", "to"][..], ">="),
            (&["less", "than", "or", "equal", "to"][..], "<="),
            (&["greater", "than"][..], ">"),
            (&["larger", "than"][..], ">"),
            (&["bigger", "than"][..], ">"),
            (&["smaller", "than"][..], "<"),
            (&["less", "than"][..], "<"),
            (&["at", "least"][..], ">="),
            (&["at", "most"][..], "<="),
        ] {
            if words.iter().enumerate().all(|(n, w)| {
                self.tokens
                    .get(index + n)
                    .is_some_and(|t| !t.quoted && t.lower == *w)
            }) {
                return Some((op, words.len()));
            }
        }
        match self.tokens.get(index)?.lower.as_str() {
            "over" | "above" | ">" => Some((">", 1)),
            "under" | "below" | "<" => Some(("<", 1)),
            ">=" => Some((">=", 1)),
            "<=" => Some(("<=", 1)),
            "exactly" | "=" | "equals" => Some(("=", 1)),
            _ => None,
        }
    }
    fn comparator(&self) -> Option<(&'static str, usize)> {
        self.comparator_at(self.at)
    }
    fn qualified_size(&mut self) -> bool {
        let word = self.word(0);
        let (field, prefix) = match word {
            "apparent" | "logical" if self.word(1) == "size" => ("apparent", 2),
            "allocated" | "on-disk" if self.word(1) == "size" => ("size", 2),
            "on" if self.phrase(&["on", "disk", "size"]) => ("size", 3),
            "size" if self.phrase(&["size", "on", "disk"]) => ("size", 3),
            "apparent" | "logical" | "allocated" | "on-disk" | "on" => {
                self.incomplete("Missing or conflicting size basis; use apparent size, logical size, allocated size, or size on disk.");
                self.at += 1;
                return true;
            }
            _ => return false,
        };
        let condition = self.at + prefix;
        if let Some((first, second, used)) = self.size_range_at(condition) {
            if size_in_bytes(&first) > size_in_bytes(&second) {
                self.incomplete("The start size is greater than the end size.");
            }
            self.clauses.push(Clause::parsed(&format!(
                "{field} >= {first} and {field} <= {second}"
            )));
            self.record(
                EntityKind::Size,
                prefix + used,
                format!("{field} {first}..{second}"),
            );
            return true;
        }
        if let Some((op, count)) = self.comparator_at(condition) {
            if let Some((amount, used)) = self.size_at(condition + count) {
                self.clauses
                    .push(Clause::parsed(&format!("{field} {op} {amount}")));
                self.record(
                    EntityKind::Size,
                    prefix + count + used,
                    format!("{field} {op} {amount}"),
                );
                return true;
            }
            if let Some(quantity) = self.quantity_at(condition + count)
                && self
                    .tokens
                    .get(condition + count + quantity.used)
                    .is_some_and(|t| {
                        !t.quoted && matches!(t.lower.as_str(), "files" | "items" | "children")
                    })
            {
                self.incomplete("A size basis requires byte units; file and child counts are separate conditions.");
                self.at += prefix + count + quantity.used + 1;
                return true;
            }
        }
        self.incomplete("Incomplete or conflicting qualified size condition; add a comparison or a byte-size range.");
        self.at += prefix;
        true
    }
    fn size_range_at(&self, index: usize) -> Option<(String, String, usize)> {
        let word = |index: usize| {
            self.tokens
                .get(index)
                .filter(|t| !t.quoted)
                .map_or("", |t| t.lower.as_str())
        };
        if word(index) != "between" {
            return None;
        }
        let first = self.size_at(index + 1).or_else(|| {
            // Validate both whole groups so a cardinal's internal "and"
            // cannot consume the range separator (one hundred and two hundred).
            (2..=(self.tokens.len() - index).min(25))
                .rev()
                .find_map(|separator| {
                    if !matches!(word(index + separator), "and" | "to") {
                        return None;
                    }
                    let quantity = numbers::parse(
                        self.tokens[index + 1..index + separator]
                            .iter()
                            .map(|t| if t.quoted { "" } else { t.lower.as_str() }),
                    )?;
                    if quantity.used != separator - 1 {
                        return None;
                    }
                    let (second, _) = self.size_at(index + separator + 1)?;
                    let suffix =
                        second.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.');
                    quantity.scaled_integer(size_scale(suffix))?;
                    Some((format!("{}{suffix}", quantity.decimal()), quantity.used))
                })
        })?;
        let separator = index + 1 + first.1;
        if !matches!(word(separator), "and" | "to") {
            return None;
        }
        let (second, used) = self.size_at(separator + 1)?;
        Some((first.0, second, first.1 + used + 2))
    }
    fn size_at(&self, index: usize) -> Option<(String, usize)> {
        let (quantity, suffix, used) = self.quantity_unit_at(index)?;
        let amount = quantity.decimal();
        let suffix = match suffix {
            "b" | "byte" | "bytes" => "b",
            "k" | "kb" | "kib" | "kilobyte" | "kilobytes" => "kib",
            "m" | "mb" | "mib" | "megabyte" | "megabytes" => "mib",
            "g" | "gb" | "gib" | "gigabyte" | "gigabytes" => "gib",
            "t" | "tb" | "tib" | "terabyte" | "terabytes" => "tib",
            _ => return None,
        };
        quantity.scaled_integer(size_scale(suffix))?;
        Some((format!("{amount}{suffix}"), used))
    }
    fn quantity_at(&self, index: usize) -> Option<Quantity> {
        numbers::parse(
            self.tokens
                .get(index..)?
                .iter()
                .map(|t| if t.quoted { "" } else { t.lower.as_str() }),
        )
    }
    fn quantity_unit_at(&self, index: usize) -> Option<(Quantity, &str, usize)> {
        let token = self.tokens.get(index)?;
        if token.quoted {
            return None;
        }
        let split = token
            .lower
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(token.lower.len());
        if split > 0 && split < token.lower.len() {
            let amount = numbers::parse(std::iter::once(&token.lower[..split]))?;
            return Some((amount, &token.lower[split..], 1));
        }
        let amount = self.quantity_at(index)?;
        let unit = self.tokens.get(index + amount.used)?;
        if unit.quoted {
            return None;
        }
        let used = amount.used + 1;
        Some((amount, unit.lower.as_str(), used))
    }
    fn date(&mut self) -> bool {
        if civil_days(self.context.local_date).is_none() {
            self.incomplete("Invalid local calendar context.");
            self.at += 1;
            return true;
        }
        let created = matches!(self.word(0), "created" | "creation" | "born");
        let field = if created {
            Field::Created
        } else {
            Field::Modified
        };
        let kind = if created {
            EntityKind::Created
        } else {
            EntityKind::Modified
        };
        let name = if created { "created" } else { "modified" };
        let mut offset = 1;
        if self.word(offset) == "date" {
            offset += 1;
        }
        if self.comparative_timestamp_age(field, kind, name, offset) {
            return true;
        }
        if self.word(offset) == "between" {
            if let Some((first, from, first_used)) = self.parse_date_at(self.at + offset + 1)
                && matches!(self.word(offset + 1 + first_used), "and" | "to")
                && let Some((_, to, second_used)) =
                    self.parse_date_at(self.at + offset + 2 + first_used)
            {
                if from > to {
                    self.incomplete("The start date is after the end date.");
                }
                let end = date_for_days(to + 1);
                self.add_date(field, name, Op::Ge, ">=", &first, from);
                self.add_date(field, name, Op::Lt, "<", &end, to + 1);
                self.record(
                    kind,
                    offset + 2 + first_used + second_used,
                    format!("{first}..{end}"),
                );
                return true;
            }
            self.incomplete("Incomplete date range; use between YYYY-MM-DD and YYYY-MM-DD.");
            return false;
        }
        if self.word(offset) == "on"
            && self.word(offset + 1) == "or"
            && matches!(self.word(offset + 2), "before" | "after")
            && let Some((date, day, used)) = self.parse_date_at(self.at + offset + 3)
        {
            let before = self.word(offset + 2) == "before";
            let (date, day, op, spelling) = if before {
                (date_for_days(day + 1), day + 1, Op::Lt, "<")
            } else {
                (date, day, Op::Ge, ">=")
            };
            self.add_date(field, name, op, spelling, &date, day);
            self.record(kind, offset + 3 + used, format!("{spelling} {date}"));
            return true;
        }
        let op = match self.word(offset) {
            "before" => Some((Op::Lt, "<")),
            "after" => Some((Op::Gt, ">")),
            "since" => Some((Op::Ge, ">=")),
            _ => None,
        };
        if let Some((op, spelling)) = op
            && let Some((date, days, used)) = self.parse_date_at(self.at + offset + 1)
        {
            self.add_date(field, name, op, spelling, &date, days);
            self.record(kind, offset + 1 + used, format!("{spelling} {date}"));
            return true;
        }
        let named_period = self.word(offset) == "in";
        if matches!(self.word(offset), "on" | "in") {
            offset += 1;
        }
        if self.word(offset) == "the" {
            offset += 1;
        }
        let local = self.context.local_date;
        let today = civil_days(local);
        let word = self.word(offset);
        let calendar_period =
            calendar::parse_relative(self.calendar_words(self.at + offset), local).or_else(|| {
                named_period
                    .then(|| calendar::parse_named_period(self.calendar_words(self.at + offset)))
                    .flatten()
            });
        if let Some(period) = calendar_period {
            let from = date_for_days(period.from);
            let to = date_for_days(period.to);
            self.add_date(field, name, Op::Ge, ">=", &from, period.from);
            self.add_date(field, name, Op::Lt, "<", &to, period.to);
            self.record(kind, offset + period.used, format!("{from}..{to}"));
            return true;
        }
        if matches!(word, "last" | "past")
            && let Some((duration, used)) = self.duration_at(self.at + offset + 1)
        {
            let now = u64::from(self.context.now);
            if let Some(lower) = now.checked_sub(duration) {
                for (op, spelling, timestamp) in [(Op::Ge, ">=", lower), (Op::Le, "<=", now)] {
                    self.clauses.push(Clause {
                        dsl: format!("{name} {spelling} @{timestamp}"),
                        expr: Expr::Compare {
                            field,
                            op,
                            value: Value::Number(timestamp),
                        },
                    });
                }
                self.record(kind, offset + 1 + used, format!("last {duration}s"));
                return true;
            }
            self.incomplete("This duration reaches before the supported Unix epoch.");
            self.record(kind, offset + 1 + used, format!("last {duration}s"));
            return true;
        }
        let range = if word == "today" {
            today.map(|d| (d, d + 1, 1))
        } else if word == "yesterday" {
            today.map(|d| (d - 1, d, 1))
        } else if word == "last" && self.word(offset + 1) == "week" {
            today.map(|d| {
                let monday = d - (d + 3).rem_euclid(7);
                (monday - 7, monday, 2)
            })
        } else if word == "last" && self.word(offset + 1) == "month" {
            let [year, month, _] = local;
            let prev = if month == 1 {
                [year - 1, 12, 1]
            } else {
                [year, month - 1, 1]
            };
            civil_days(prev)
                .zip(civil_days([year, month, 1]))
                .map(|(a, b)| (a, b, 2))
        } else if word == "last" && self.word(offset + 1) == "year" {
            civil_days([local[0] - 1, 1, 1])
                .zip(civil_days([local[0], 1, 1]))
                .map(|(a, b)| (a, b, 2))
        } else if word == "this" && self.word(offset + 1) == "week" {
            today.map(|d| {
                let monday = d - (d + 3).rem_euclid(7);
                (monday, monday + 7, 2)
            })
        } else if word == "this" && self.word(offset + 1) == "month" {
            let [year, month, _] = local;
            let next = if month == 12 {
                [year + 1, 1, 1]
            } else {
                [year, month + 1, 1]
            };
            civil_days([year, month, 1])
                .zip(civil_days(next))
                .map(|(a, b)| (a, b, 2))
        } else if word == "this" && self.word(offset + 1) == "year" {
            civil_days([local[0], 1, 1])
                .zip(civil_days([local[0] + 1, 1, 1]))
                .map(|(a, b)| (a, b, 2))
        } else if let Some((_, day, used)) = self.parse_date_at(self.at + offset) {
            Some((day, day + 1, used))
        } else {
            None
        };
        if let Some((from, to, used)) = range {
            let from_date = date_for_days(from);
            let to_date = date_for_days(to);
            self.add_date(field, name, Op::Ge, ">=", &from_date, from);
            self.add_date(field, name, Op::Lt, "<", &to_date, to);
            self.record(kind, offset + used, format!("{from_date}..{to_date}"));
            return true;
        }
        self.structured = true;
        self.diagnostics.push(Diagnostic {
            span: self.tokens[self.at].span.clone(),
            message: format!("Incomplete or unsupported {name} date."),
            hint: "Use before/after/since YYYY-MM-DD, today, yesterday, or an exact date.".into(),
        });
        self.at += 1;
        true
    }
    fn comparative_timestamp_age(
        &mut self,
        field: Field,
        kind: EntityKind,
        name: &str,
        offset: usize,
    ) -> bool {
        let Some((comparison, comparison_used)) = self.comparator_at(self.at + offset) else {
            return false;
        };
        let (op, spelling) = match comparison {
            ">" => (Op::Lt, "<"),
            ">=" => (Op::Le, "<="),
            "<" => (Op::Gt, ">"),
            "<=" => (Op::Ge, ">="),
            _ => {
                self.incomplete("Unsupported comparative date operator; use more than, less than, at least, or at most a duration ago.");
                self.at += offset + comparison_used;
                return true;
            }
        };
        let Some((duration, used)) = self.duration_at(self.at + offset + comparison_used) else {
            self.incomplete("Comparative date conditions need an exact duration in seconds, minutes, hours, days, or weeks, followed by ago.");
            self.at += offset + comparison_used;
            return true;
        };
        let ago = offset + comparison_used + used;
        if self.word(ago) != "ago" {
            self.incomplete(
                "Comparative date conditions require the word ago immediately after the duration.",
            );
            self.at += ago;
            return true;
        }
        let now = u64::from(self.context.now);
        if let Some(boundary) = now.checked_sub(duration) {
            for (op, spelling, timestamp) in [(op, spelling, boundary), (Op::Le, "<=", now)] {
                self.clauses.push(Clause {
                    dsl: format!("{name} {spelling} @{timestamp}"),
                    expr: Expr::Compare {
                        field,
                        op,
                        value: Value::Number(timestamp),
                    },
                });
            }
        } else {
            self.incomplete("This duration reaches before the supported Unix epoch.");
        }
        self.record(kind, ago + 1, format!("age {comparison} {duration}s"));
        true
    }
    fn calendar_words(&self, index: usize) -> impl Iterator<Item = &str> {
        self.tokens[index..]
            .iter()
            .map(|t| if t.quoted { "" } else { t.lower.as_str() })
    }
    fn parse_date_at(&self, index: usize) -> Option<(String, i64, usize)> {
        let date = calendar::parse_date(self.calendar_words(index))?;
        Some((date_for_days(date.days), date.days, date.used))
    }
    fn add_date(
        &mut self,
        field: Field,
        name: &str,
        op: Op,
        spelling: &str,
        date: &str,
        days: i64,
    ) {
        let parts: Option<Vec<i32>> = date.split('-').map(|p| p.parse().ok()).collect();
        let timestamp = parts
            .filter(|p| p.len() == 3 && p[0] >= 1970 && p[0] <= 9999 && days >= 0)
            .and_then(|p| (self.resolve_date)([p[0], p[1], p[2]]));
        let Some(timestamp) = timestamp else {
            self.diagnostics.push(Diagnostic {
                span: self.tokens[self.at].span.clone(),
                message: "This local date could not be resolved.".into(),
                hint: "Use a supported date from 1970 onwards.".into(),
            });
            return;
        };
        self.clauses.push(Clause {
            dsl: format!("{name} {spelling} {date}"),
            expr: Expr::Compare {
                field,
                op,
                value: Value::Number(timestamp),
            },
        });
    }
}

fn size_scale(suffix: &str) -> u64 {
    match suffix {
        "kib" => 1024,
        "mib" => 1048576,
        "gib" => 1073741824,
        "tib" => 1099511627776,
        _ => 1,
    }
}

fn size_in_bytes(value: &str) -> u64 {
    let split = value
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(value.len());
    numbers::parse(std::iter::once(&value[..split]))
        .expect("validated size")
        .scaled_integer(size_scale(&value[split..]))
        .expect("validated byte quantity")
}

pub(crate) fn common_extensions() -> impl Iterator<Item = &'static str> {
    [
        "markdown",
        "c",
        "h",
        "cpp",
        "rust",
        "python",
        "javascript",
        "text",
        "pdf",
        "word",
        "excel",
        "documents",
        "images",
        "video",
        "audio",
        "media",
        "archives",
        "json",
        "csv",
        "code",
        "typescript",
        "java",
        "go",
        "shell",
        "powershell",
        "sql",
        "config",
        "presentations",
        "ebooks",
        "fonts",
        "database",
        "logs",
        "executables",
    ]
    .into_iter()
    .flat_map(|word| extensions(word).expect("dictionary family").iter().copied())
}

fn extensions(word: &str) -> Option<&'static [&'static str]> {
    Some(match word.strip_prefix('.').unwrap_or(word) {
        "md" => &["md"],
        "markdown" => &["md", "markdown"],
        "c" => &["c"],
        "h" => &["h"],
        "cpp" | "c++" => &["cpp", "cc", "cxx", "h", "hpp", "hxx"],
        "rust" | "rs" => &["rs"],
        "python" | "py" => &["py"],
        "javascript" | "js" => &["js", "jsx", "mjs", "cjs"],
        "typescript" | "ts" => &["ts", "tsx", "mts", "cts"],
        "java" => &["java"],
        "kotlin" => &["kt", "kts"],
        "go" => &["go"],
        "csharp" | "c#" => &["cs"],
        "ruby" => &["rb", "rake"],
        "swift" => &["swift"],
        "php" => &["php"],
        "shell" => &["sh", "bash", "zsh", "fish"],
        "powershell" => &["ps1", "psm1", "psd1"],
        "sql" => &["sql"],
        "code" => &[
            "c", "h", "cpp", "cc", "cxx", "hpp", "rs", "py", "js", "jsx", "ts", "tsx", "java",
            "kt", "go", "cs", "rb", "swift", "php", "sh", "ps1", "sql",
        ],
        "config" | "configuration" => &["toml", "yaml", "yml", "ini", "cfg", "conf", "json", "xml"],
        "presentation" | "presentations" | "powerpoint" => {
            &["ppt", "pptx", "pptm", "pps", "ppsx", "odp", "key"]
        }
        "ebook" | "ebooks" => &["epub", "mobi", "azw", "azw3", "pdf"],
        "font" | "fonts" => &["ttf", "otf", "woff", "woff2"],
        "database" | "databases" => &["db", "sqlite", "sqlite3", "mdb", "accdb", "sql"],
        "log" | "logs" => &["log"],
        "executable" | "executables" => &[
            "exe", "dll", "com", "msi", "msp", "sys", "bat", "cmd", "ps1", "sh", "bin",
        ],
        "text" | "txt" => &["txt"],
        "pdf" | "pdfs" => &["pdf"],
        "word" => &["doc", "docx", "docm", "dot", "dotx", "dotm", "rtf"],
        "excel" | "spreadsheet" | "spreadsheets" | "sheet" | "sheets" => &[
            "xls", "xlsx", "xlsm", "xlsb", "xlt", "xltx", "xltm", "ods", "csv", "tsv",
        ],
        "document" | "documents" => &["pdf", "doc", "docx", "docm", "odt", "rtf", "txt", "md"],
        "image" | "images" | "photo" | "photos" | "picture" | "pictures" => &[
            "jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "svg", "heic", "avif", "ico",
        ],
        "video" | "videos" => &[
            "mp4", "mkv", "mov", "avi", "wmv", "webm", "m4v", "mpeg", "mpg",
        ],
        "audio" | "music" => &["mp3", "wav", "flac", "aac", "ogg", "m4a", "wma", "opus"],
        "media" => &[
            "jpg", "jpeg", "png", "gif", "webp", "mp4", "mkv", "mov", "avi", "webm", "mp3", "wav",
            "flac", "aac", "ogg", "m4a",
        ],
        "archive" | "archives" | "compressed" => {
            &["zip", "7z", "rar", "tar", "gz", "bz2", "xz", "zst", "tgz"]
        }
        "zip" => &["zip"],
        "json" => &["json"],
        "csv" => &["csv"],
        "doc" => &["doc"],
        "docx" => &["docx"],
        "xlsx" => &["xlsx"],
        "png" => &["png"],
        "jpg" => &["jpg"],
        "mp4" => &["mp4"],
        _ => return None,
    })
}
