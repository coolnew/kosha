// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Turning text into an [`Expr`].
//!
//! A hand-written lexer and a recursive-descent parser. The grammar is small
//! enough that a parser generator would be more machinery than language:
//!
//! ```text
//! query      := or
//! or         := and ( ("or" | "||") and )*
//! and        := unary ( ("and" | "&&" | ",")? unary )*
//! unary      := ("not" | "!") unary | primary
//! primary    := "(" or ")" | flag | comparison
//! comparison := field op value | field "in" "(" value ("," value)* ")"
//! ```
//!
//! Adjacency means `and`, so `size > 1gb ext = mp4` works and reads naturally.
//! That is the only implicit thing in the grammar; everything else has to be
//! written out, because a filter that guesses is a filter that silently returns
//! the wrong set.

use std::fmt;
use std::ops::Range;

use crate::{Expr, Field, Flag, Op, Value};

pub type Span = Range<usize>;

/// What kind of thing went wrong.
///
/// Typed rather than left to the message, so that suggestions can act on the
/// specific problem instead of matching on prose that is free to change.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum ErrorKind {
    /// Nothing to parse.
    Empty,
    /// A word where a field was expected.
    UnknownField { word: String },
    /// A field with no comparison after it.
    MissingOperator { field: String },
    /// An operator with nothing after it.
    MissingValue,
    /// A value that is not a size, duration, date or number.
    BadValue { field: String, raw: String },
    /// A substring operator used on a numeric field.
    TextOperatorOnNumber { field: String, raw: String },
    /// A text value that cannot match anything as written.
    ///
    /// `instead` is the value to put in its place where there is one — there is
    /// not always, because `name = *.txt` means a comparison this language does
    /// not have and the fix is to change the operator as well.
    UnmatchableText {
        field: String,
        raw: String,
        instead: Option<String>,
    },
    /// A bracket, quote or list left open.
    Unclosed,
    #[default]
    Other,
}

/// A parse failure, with the exact text that caused it.
#[derive(Clone, PartialEq, Debug)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
    /// What was expected here, when we can say usefully.
    pub hint: Option<String>,
    /// The specific problem, for a suggester to act on.
    pub kind: ErrorKind,
    source: String,
}

impl ParseError {
    fn at(source: &str, span: Span, message: impl Into<String>) -> ParseError {
        ParseError {
            message: message.into(),
            span,
            hint: None,
            kind: ErrorKind::Other,
            source: source.to_string(),
        }
    }

    fn of(mut self, kind: ErrorKind) -> ParseError {
        self.kind = kind;
        self
    }

    fn with_hint(mut self, hint: impl Into<String>) -> ParseError {
        self.hint = Some(hint.into());
        self
    }

    /// The offending text, for a UI that wants to underline it.
    pub fn text(&self) -> &str {
        self.source.get(self.span.clone()).unwrap_or("")
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, " ({hint})")?;
        }
        Ok(())
    }
}

impl std::error::Error for ParseError {}

#[derive(Clone, PartialEq, Debug)]
enum Token {
    Word(String),
    /// A quoted string; never interpreted as a keyword or a number.
    Quoted(String),
    Op(Op),
    LParen,
    RParen,
    Comma,
}

#[derive(Clone, Debug)]
struct Spanned {
    token: Token,
    span: Span,
}

fn lex(input: &str) -> Result<Vec<Spanned>, ParseError> {
    let bytes = input.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        let start = i;
        let c = bytes[i];

        match c {
            b' ' | b'\t' | b'\n' | b'\r' => {
                i += 1;
            }
            b'(' => {
                tokens.push(Spanned {
                    token: Token::LParen,
                    span: start..i + 1,
                });
                i += 1;
            }
            b')' => {
                tokens.push(Spanned {
                    token: Token::RParen,
                    span: start..i + 1,
                });
                i += 1;
            }
            b',' => {
                tokens.push(Spanned {
                    token: Token::Comma,
                    span: start..i + 1,
                });
                i += 1;
            }
            b'"' | b'\'' => {
                let quote = c;
                i += 1;
                let text_start = i;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                if i >= bytes.len() {
                    return Err(ParseError::at(input, start..bytes.len(), "unclosed quote")
                        .with_hint("add a closing quote")
                        .of(ErrorKind::Unclosed));
                }
                let text = input[text_start..i].to_string();
                i += 1;
                tokens.push(Spanned {
                    token: Token::Quoted(text),
                    span: start..i,
                });
            }
            b'>' | b'<' | b'=' | b'!' | b'~' | b'&' | b'|' => {
                let (token, len) = lex_operator(bytes, i, input, start)?;
                tokens.push(Spanned {
                    token,
                    span: start..i + len,
                });
                i += len;
            }
            _ => {
                while i < bytes.len() && !is_delimiter(bytes[i]) {
                    i += 1;
                }
                tokens.push(Spanned {
                    token: Token::Word(input[start..i].to_string()),
                    span: start..i,
                });
            }
        }
    }
    Ok(tokens)
}

/// Read one operator, and say how many bytes it took.
///
/// `&&`, `||` and a lone `!` desugar to the keywords `and`, `or` and `not`
/// rather than to operators, which keeps the parser to one spelling of each.
///
/// They must desugar to the *right* keyword. An earlier version emitted no
/// token for either `&&` or `||`, on the reasoning that adjacency already means
/// `and` — true for `&&`, and quietly wrong for `||`, which turned every `a ||
/// b` into `a and b` and returned nothing.
fn lex_operator(
    bytes: &[u8],
    i: usize,
    input: &str,
    start: usize,
) -> Result<(Token, usize), ParseError> {
    let word = |text: &str| Token::Word(text.to_string());
    let next = bytes.get(i + 1).copied();

    Ok(match (bytes[i], next) {
        (b'>', Some(b'=')) => (Token::Op(Op::Ge), 2),
        (b'<', Some(b'=')) => (Token::Op(Op::Le), 2),
        (b'!', Some(b'=')) => (Token::Op(Op::Ne), 2),
        (b'!', Some(b'~')) => (Token::Op(Op::NotContains), 2),
        (b'=', Some(b'=')) => (Token::Op(Op::Eq), 2),
        (b'&', Some(b'&')) => (word("and"), 2),
        (b'|', Some(b'|')) => (word("or"), 2),
        (b'>', _) => (Token::Op(Op::Gt), 1),
        (b'<', _) => (Token::Op(Op::Lt), 1),
        (b'=', _) => (Token::Op(Op::Eq), 1),
        (b'~', _) => (Token::Op(Op::Contains), 1),
        // A lone `!` is negation. `!~` covers "does not contain", so there is
        // nothing else for it to mean.
        (b'!', _) => (word("not"), 1),
        (b'&', _) | (b'|', _) => {
            return Err(ParseError::at(
                input,
                start..i + 1,
                format!("`{}` on its own is not an operator", bytes[i] as char),
            )
            .with_hint(format!("did you mean `{0}{0}`?", bytes[i] as char)));
        }
        (c, _) => {
            return Err(ParseError::at(
                input,
                start..i + 1,
                format!("`{}` does not start an operator", c as char),
            ));
        }
    })
}

fn is_delimiter(c: u8) -> bool {
    matches!(
        c,
        b' ' | b'\t' | b'\n' | b'\r' | b'(' | b')' | b',' | b'>' | b'<' | b'=' | b'!' | b'~'
    )
}

/// How deeply brackets and `not` may nest.
///
/// **This is a stack limit wearing a syntax limit's clothes.** The parser is
/// recursive descent, so one bracket is one frame, and `(((…)))` five thousand
/// deep overflowed the stack: the process died with `STATUS_STACK_OVERFLOW` and
/// printed nothing kosha wrote. There is no error to catch and nothing to
/// report — in the window, which parses on every keystroke, it is the whole
/// application vanishing while somebody is typing.
///
/// Sixty-four is past anything a person writes and nowhere near the stack. The
/// deepest query in the catalogue nests twice.
const MAX_NESTING: usize = 64;

/// How many conditions may be joined with `and` and `or` in one query.
///
/// A separate limit because it is a different failure with the same cause.
/// Chaining does not recurse while *parsing* — `parse_and` loops — but it
/// builds a tree that leans one way, and everything that walks one afterwards
/// does recurse: dropping it, evaluating it. Fifty thousand terms parsed
/// happily and then overflowed the stack on the way out of scope, which is a
/// crash in a destructor and about as far from the cause as a failure can land.
///
/// A thousand and twenty-four, rather than something nearer the edge, because
/// the edge moves: a test thread, the window's render thread and the main
/// thread do not all get the same stack, and neither do three operating
/// systems. Nobody writes a thousand conditions by hand, and a generated query
/// that wants a long alternation should use `in (…)`, which is one condition.
const MAX_CONDITIONS: usize = 1024;

struct Parser<'a> {
    tokens: Vec<Spanned>,
    at: usize,
    input: &'a str,
    /// Brackets and `not` currently open. See [`MAX_NESTING`].
    depth: usize,
    /// Conditions joined so far. See [`MAX_CONDITIONS`].
    conditions: usize,
}

pub fn parse(input: &str) -> Result<Expr, ParseError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ParseError::at(input, 0..0, "the query is empty").of(ErrorKind::Empty));
    }

    let tokens = lex(input)?;

    // The shorthand: text with nothing structural in it is a name search. It is
    // what an ordinary filter box means, and it keeps the syntax out of the way
    // until somebody wants it.
    if tokens.iter().all(|t| matches!(t.token, Token::Word(_)))
        && !tokens.iter().any(|t| is_structural_word(&t.token))
    {
        return Ok(Expr::NameContains(trimmed.to_string()));
    }
    if let [
        Spanned {
            token: Token::Quoted(text),
            ..
        },
    ] = tokens.as_slice()
    {
        return Ok(Expr::NameContains(text.clone()));
    }

    let mut parser = Parser {
        tokens,
        at: 0,
        input,
        depth: 0,
        conditions: 0,
    };
    let expr = parser.parse_or()?;
    if let Some(extra) = parser.peek().cloned() {
        return Err(
            ParseError::at(input, extra.span, "unexpected trailing input")
                .with_hint("join conditions with `and` or `or`"),
        );
    }
    Ok(expr)
}

/// Words that mean the text is a query rather than a name to look for.
fn is_structural_word(token: &Token) -> bool {
    let Token::Word(word) = token else {
        return false;
    };
    let lower = word.to_ascii_lowercase();
    matches!(lower.as_str(), "and" | "or" | "not" | "in")
        || field_from_word(&lower).is_some()
        || flag_from_word(&lower).is_some()
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Spanned> {
        self.tokens.get(self.at)
    }

    fn next(&mut self) -> Option<Spanned> {
        let token = self.tokens.get(self.at).cloned();
        if token.is_some() {
            self.at += 1;
        }
        token
    }

    /// The span to blame when we run out of input.
    fn end_span(&self) -> Span {
        self.tokens
            .last()
            .map(|t| t.span.end..t.span.end)
            .unwrap_or(0..0)
    }

    fn eat_word(&mut self, want: &str) -> bool {
        match self.peek() {
            Some(Spanned {
                token: Token::Word(word),
                ..
            }) if word.eq_ignore_ascii_case(want) => {
                self.at += 1;
                true
            }
            _ => false,
        }
    }

    /// One more level of nesting, or a refusal saying so.
    fn go_deeper(&mut self, span: Span) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_NESTING {
            return Err(ParseError::at(
                self.input,
                span,
                format!("this nests more than {MAX_NESTING} brackets deep"),
            )
            .with_hint("take the inner conditions out into a query of their own"));
        }
        Ok(())
    }

    /// One more condition joined, or a refusal saying so.
    fn one_more_condition(&mut self, span: Span) -> Result<(), ParseError> {
        self.conditions += 1;
        if self.conditions > MAX_CONDITIONS {
            return Err(ParseError::at(
                self.input,
                span,
                format!("this joins more than {MAX_CONDITIONS} conditions"),
            )
            .with_hint("a long list of alternatives is one condition: `ext in (mp4, mkv, mov)`"));
        }
        Ok(())
    }

    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_and()?;
        while self.eat_word("or") {
            let span = self.end_span();
            self.one_more_condition(span)?;
            let right = self.parse_and()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.parse_unary()?;
        loop {
            // `and` may be written, or implied by adjacency. A comma between
            // conditions reads as `and` too, which is how people write lists.
            let explicit = self.eat_word("and");
            if !explicit {
                if matches!(self.peek().map(|t| &t.token), Some(Token::Comma)) {
                    self.at += 1;
                } else if !self.starts_operand() {
                    break;
                }
            }
            let span = self.end_span();
            self.one_more_condition(span)?;
            let right = self.parse_unary()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// Whether the next token could begin another condition.
    fn starts_operand(&self) -> bool {
        match self.peek().map(|t| &t.token) {
            Some(Token::LParen | Token::Quoted(_)) => true,
            Some(Token::Word(word)) => !word.eq_ignore_ascii_case("or"),
            _ => false,
        }
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        if self.eat_word("not") {
            let span = self.end_span();
            self.go_deeper(span)?;
            let inner = self.parse_unary()?;
            self.depth -= 1;
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        let Some(token) = self.next() else {
            return Err(
                ParseError::at(self.input, self.end_span(), "the query ends early")
                    .with_hint("something is missing after the last operator"),
            );
        };

        match &token.token {
            Token::LParen => {
                self.go_deeper(token.span.clone())?;
                let inner = self.parse_or()?;
                self.depth -= 1;
                match self.next() {
                    Some(Spanned {
                        token: Token::RParen,
                        ..
                    }) => Ok(inner),
                    Some(other) => Err(ParseError::at(
                        self.input,
                        other.span,
                        "expected a closing bracket",
                    )),
                    None => Err(
                        ParseError::at(self.input, self.end_span(), "unclosed bracket")
                            .of(ErrorKind::Unclosed),
                    ),
                }
            }
            Token::Word(word) => self.parse_word(word, token.span.clone()),
            Token::Quoted(text) => Ok(Expr::NameContains(text.clone())),
            Token::Op(_) => Err(ParseError::at(
                self.input,
                token.span,
                "an operator needs a field before it",
            )
            .with_hint("try `size > 1gb`")),
            Token::RParen | Token::Comma => Err(ParseError::at(
                self.input,
                token.span,
                "unexpected punctuation",
            )),
        }
    }

    fn parse_word(&mut self, word: &str, span: Span) -> Result<Expr, ParseError> {
        let lower = word.to_ascii_lowercase();

        if let Some(flag) = flag_from_word(&lower) {
            return Ok(Expr::Flag(flag));
        }

        let Some(field) = field_from_word(&lower) else {
            return Err(
                ParseError::at(self.input, span, format!("`{word}` is not a field"))
                    .with_hint(nearest_field(&lower))
                    .of(ErrorKind::UnknownField {
                        word: word.to_string(),
                    }),
            );
        };

        // `ext in (mp4, mkv)`
        if self.eat_word("in") {
            return self.parse_in(field, false);
        }
        if self.eat_word("not") {
            if self.eat_word("in") {
                return self.parse_in(field, true);
            }
            return Err(ParseError::at(
                self.input,
                self.end_span(),
                "expected `in` after `not`",
            ));
        }

        let Some(next) = self.next() else {
            return Err(ParseError::at(
                self.input,
                self.end_span(),
                format!("`{}` needs a comparison", field.name()),
            )
            .with_hint(format!("try `{} > 100mb`", field.name()))
            .of(ErrorKind::MissingOperator {
                field: field.name().to_string(),
            }));
        };
        let op = match &next.token {
            Token::Op(op) => *op,
            Token::Word(word) if word.eq_ignore_ascii_case("startswith") => Op::StartsWith,
            Token::Word(word) if word.eq_ignore_ascii_case("endswith") => Op::EndsWith,
            _ => {
                return Err(ParseError::at(
                    self.input,
                    next.span,
                    format!("expected an operator after `{}`", field.name()),
                )
                .with_hint("one of > >= < <= = != ~ !~, startswith, endswith, or `in`")
                .of(ErrorKind::MissingOperator {
                    field: field.name().to_string(),
                }));
            }
        };

        let value = self.parse_value(field, op)?;
        Ok(Expr::Compare { field, op, value })
    }

    fn parse_in(&mut self, field: Field, negated: bool) -> Result<Expr, ParseError> {
        match self.next() {
            Some(Spanned {
                token: Token::LParen,
                ..
            }) => {}
            Some(other) => {
                return Err(ParseError::at(
                    self.input,
                    other.span,
                    "expected a bracketed list after `in`",
                )
                .with_hint("try `ext in (mp4, mkv)`"));
            }
            None => {
                return Err(ParseError::at(
                    self.input,
                    self.end_span(),
                    "expected a list after `in`",
                ));
            }
        }

        let mut values = Vec::new();
        loop {
            let value = self.parse_value(field, Op::Eq)?;
            values.push(value);
            match self.next() {
                Some(Spanned {
                    token: Token::Comma,
                    ..
                }) => continue,
                Some(Spanned {
                    token: Token::RParen,
                    ..
                }) => break,
                Some(other) => {
                    return Err(ParseError::at(
                        self.input,
                        other.span,
                        "expected a comma or a closing bracket",
                    ));
                }
                None => {
                    return Err(ParseError::at(self.input, self.end_span(), "unclosed list")
                        .of(ErrorKind::Unclosed));
                }
            }
        }

        if values.is_empty() {
            return Err(ParseError::at(
                self.input,
                self.end_span(),
                "the list is empty",
            ));
        }
        Ok(Expr::In {
            field,
            values,
            negated,
        })
    }

    fn parse_value(&mut self, field: Field, op: Op) -> Result<Value, ParseError> {
        let Some(token) = self.next() else {
            return Err(
                ParseError::at(self.input, self.end_span(), "expected a value")
                    .of(ErrorKind::MissingValue),
            );
        };
        if !field.is_text()
            && matches!(
                op,
                Op::Contains | Op::NotContains | Op::StartsWith | Op::EndsWith
            )
        {
            let operator = match op {
                Op::StartsWith => "startswith",
                Op::EndsWith => "endswith",
                _ => "~",
            };
            let raw = match &token.token {
                Token::Word(text) | Token::Quoted(text) => text.clone(),
                _ => String::new(),
            };
            return Err(ParseError::at(
                self.input,
                token.span,
                format!(
                    "`{operator}` compares text, and `{}` is a number",
                    field.name()
                ),
            )
            .with_hint(format!("try `{} > {raw}`", field.name()))
            .of(ErrorKind::TextOperatorOnNumber {
                field: field.name().into(),
                raw,
            }));
        }
        let raw = match &token.token {
            Token::Word(word) => word.clone(),
            Token::Quoted(text) if field.is_text() => return Ok(Value::Text(text.clone())),
            Token::Quoted(text) => {
                return Err(ParseError::at(
                    self.input,
                    token.span,
                    format!(
                        "`{}` expects an unquoted numeric or date value",
                        field.name()
                    ),
                )
                .with_hint("remove the quotes around the size, count, age, or date")
                .of(ErrorKind::BadValue {
                    field: field.name().into(),
                    raw: text.clone(),
                }));
            }
            _ => {
                return Err(ParseError::at(self.input, token.span, "expected a value")
                    .of(ErrorKind::MissingValue));
            }
        };

        // A text field takes the word as written. `~` always compares text,
        // whatever the field, so `size ~ 100` is a mistake rather than a
        // silently different question.
        if field.is_text() {
            if let Some(problem) = unmatchable(field, op, &raw) {
                return Err(ParseError::at(self.input, token.span, problem.said)
                    .with_hint(problem.hint)
                    .of(ErrorKind::UnmatchableText {
                        field: field.name().to_string(),
                        raw: raw.clone(),
                        instead: problem.instead,
                    }));
            }
            return Ok(Value::Text(raw));
        }

        // **A size written the way kosha writes it.** The interface prints
        // `512 KiB`, and `size > 512 kib` came back "`kib` is not a field
        // (did you mean `age`?)" — advice about a field, for what is plainly
        // a unit, and advice that would have taken the reader further from
        // the answer. `age > 30 d` was the same.
        //
        // The space is a convention of how a value is *written*, never a
        // second term: nothing in this grammar puts two values side by side,
        // so a following word that turns the number into a valid one can only
        // have been meant as its unit. That is the whole condition, and it is
        // why the keywords need no special case — `size > 1 and name ~ x`
        // joins to `1and`, which is not a size, so nothing is taken.
        //
        // It only ever accepts what was refused before. No query that parsed
        // means anything different.
        let (raw, span) = match self.peek() {
            Some(next) => match &next.token {
                Token::Word(unit)
                    if parse_number(field, &format!("{raw}{unit}")).is_some()
                        && parse_number(field, unit).is_none() =>
                {
                    let joined = format!("{raw}{unit}");
                    let span = token.span.start..next.span.end;
                    self.at += 1;
                    (joined, span)
                }
                _ => (raw, token.span),
            },
            None => (raw, token.span),
        };

        parse_number(field, &raw).ok_or_else(|| {
            ParseError::at(self.input, span, number_problem(field, &raw)).of(ErrorKind::BadValue {
                field: field.name().to_string(),
                raw: raw.clone(),
            })
        })
    }
}

/// What is wrong with a text value that could never match, and what to do.
struct Unmatchable {
    said: String,
    hint: String,
    /// The value to put in its place, where one value can express the fix.
    instead: Option<String>,
}

/// A text comparison that cannot match anything, whatever is on the disk.
///
/// **Returning nothing is an answer, and it was the wrong one.** Two spellings
/// that anybody would try came back `0 matched` with no explanation, in a
/// language whose own design note says a filter that guesses quietly returns
/// the wrong set:
///
/// ```text
/// name = *.txt      a glob, which is what a shell has taught everybody to type
/// ext = .txt        the dot, which the table beside the box shows in every row
/// ```
///
/// Neither can ever match: no name contains a `*` on Windows and almost never
/// on Unix, and an extension is stored without its dot. So they are refused
/// with the spelling that works, which is what this language already does for
/// `size ~ 100`.
///
/// Quoted values never reach here — [`Parser::parse_value`] returns them
/// untouched — so somebody who genuinely has a file called `*.txt` can still
/// ask for it by writing `name = "*.txt"`.
fn unmatchable(field: Field, op: Op, raw: &str) -> Option<Unmatchable> {
    let name = field.name();

    // A glob. `~` is the substring test this language has, so the wildcards are
    // simply removed for it; with `=` the fix is a different operator, which
    // one value cannot express.
    if raw.contains('*') || raw.contains('?') {
        let stripped: String = raw.chars().filter(|&c| c != '*' && c != '?').collect();
        let contains = matches!(op, Op::Contains | Op::NotContains);
        return Some(Unmatchable {
            said: format!("`{raw}` has a wildcard in it, and this is not a pattern match"),
            hint: if stripped.is_empty() {
                format!("`{name} ~ something` matches anywhere in the {name}")
            } else {
                format!("try `{name} ~ {stripped}`, which matches anywhere in the {name}")
            },
            instead: (contains && !stripped.is_empty()).then_some(stripped),
        });
    }

    // An extension with its dot. `.gitignore` is deliberately not corrected by
    // stripping: a leading dot is the whole name of a dotfile, which has no
    // extension at all, so `ext = gitignore` would answer a different question
    // rather than the same one spelled properly.
    if field == Field::Ext && raw.len() > 1 && raw.starts_with('.') {
        let bare = raw.trim_start_matches('.').to_string();
        return Some(Unmatchable {
            said: format!("`{raw}` is an extension with its dot, and they are stored without one"),
            hint: format!("try `ext = {bare}`, and `name = {raw}` for a file called that"),
            instead: Some(bare),
        });
    }

    None
}

fn number_problem(field: Field, raw: &str) -> String {
    match field {
        Field::Age => format!("`{raw}` is not a length of time"),
        // **It is a date, and saying otherwise is the mistake `--top` made
        // about a number.** A file's timestamp is seconds since 1970, so a day
        // before that cannot be one — which is a fact about what can be
        // compared, not about the spelling of what was typed. `1969-12-31` and
        // `0001-01-01` both came back as "is not a date".
        Field::Modified | Field::Created if before_the_epoch(raw) => {
            format!("`{raw}` is before 1970, and a file's date cannot be")
        }
        Field::Modified | Field::Created => format!("`{raw}` is not a date"),
        Field::Size | Field::Apparent | Field::Wasted => format!("`{raw}` is not a size"),
        _ => format!("`{raw}` is not a number"),
    }
}

/// A well-formed calendar date that the epoch cannot reach.
///
/// Only asked about a value the date parser has already refused, so this is
/// the difference between "that is not a date" and "that is a date this cannot
/// compare against" — which is the whole of what the message has to get right.
fn before_the_epoch(raw: &str) -> bool {
    let mut parts = raw.split('-');
    let (Some(year), Some(month), Some(day), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let (Ok(year), Ok(month), Ok(day)) = (year.parse::<i64>(), month.parse(), day.parse()) else {
        return false;
    };
    year < 1970 && kosha_core::format::is_a_date(year, month, day)
}

/// Parse a literal in whatever units the field is measured in.
fn parse_number(field: Field, raw: &str) -> Option<Value> {
    let value = match field {
        Field::Size | Field::Apparent | Field::Wasted => parse_size(raw)?,
        Field::Age => parse_duration(raw)?,
        Field::Modified | Field::Created => parse_date(raw)?,
        _ => raw.replace('_', "").parse::<u64>().ok()?,
    };
    Some(Value::Number(value))
}

/// `100`, `10k`, `1.5gb`, `500mib`.
///
/// Bare `kb`/`mb`/`gb` mean powers of 1024, matching what the interface shows
/// by default. Somebody who means exactly 10⁹ bytes can write `1000000000b`.
fn parse_size(raw: &str) -> Option<u64> {
    let cleaned = raw.replace('_', "").to_ascii_lowercase();
    let split = cleaned
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(cleaned.len());
    let (digits, suffix) = cleaned.split_at(split);
    let scale: u64 = match suffix.trim() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        "t" | "tb" | "tib" => 1 << 40,
        _ => return None,
    };
    scaled_decimal(digits, scale)
}

/// `30d`, `6mo`, `2y`, `12h`. Returns seconds.
fn parse_duration(raw: &str) -> Option<u64> {
    let cleaned = raw.replace('_', "").to_ascii_lowercase();
    let split = cleaned
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(cleaned.len());
    let (digits, suffix) = cleaned.split_at(split);

    // There is deliberately no bare `m`. In a duration it reads as minutes to
    // one person and months to another, and the two differ by a factor of
    // 43,200 — so `age > 6m` is rejected and the user writes `6mo` or `6min`
    // rather than getting an answer that is confidently wrong. (An earlier
    // comment here claimed `mo` was "checked before `m`", which described a
    // prefix-matching hazard this `match` does not have and an arm that does
    // not exist.)
    let seconds: u64 = match suffix.trim() {
        "s" | "sec" => 1,
        "min" => 60,
        "h" | "hr" | "hour" | "hours" => 3_600,
        "d" | "day" | "days" => 86_400,
        "w" | "week" | "weeks" => 604_800,
        "mo" | "month" | "months" => 2_592_000,
        "y" | "yr" | "year" | "years" => 31_536_000,
        // A bare number of days is the reading people expect from an age.
        "" => 86_400,
        _ => return None,
    };
    scaled_decimal(digits, seconds)
}

/// Scale a nonnegative decimal without floating-point rounding. Whole integers
/// retain every bit through u64::MAX, overflows are errors, and fractional base
/// units keep the DSL's existing truncation rule. Long fractional strings are
/// handled by decimal carry, without constructing a huge power-of-ten divisor.
fn scaled_decimal(amount: &str, scale: u64) -> Option<u64> {
    if !amount.bytes().any(|byte| byte.is_ascii_digit())
        || !amount
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return None;
    }
    let mut parts = amount.split('.');
    let whole = parts.next()?;
    let fraction = parts.next().unwrap_or("");
    if parts.next().is_some() {
        return None;
    }
    let whole = if whole.is_empty() {
        0
    } else {
        whole.parse::<u64>().ok()?
    };
    let whole = whole.checked_mul(scale)?;
    let fractional = fraction.bytes().rev().fold(0u128, |carry, digit| {
        (u128::from(digit - b'0') * u128::from(scale) + carry) / 10
    });
    whole.checked_add(u64::try_from(fractional).ok()?)
}

/// `2024-01-01`, as the Unix second that date begins on **this machine**.
///
/// Local, not UTC. A date is the one thing in this language that a person writes
/// from their own calendar, `format::stamp` prints local time in the table beside
/// it, and a boundary at midnight UTC put the machine's whole offset of every day
/// on the wrong side of it — `28 Feb 23:30` fell after `2026-03-01` for anybody
/// east of Greenwich, and before `2026-02-28` for anybody west.
///
/// UTC is the fallback, for a platform that will not say what local time is. That
/// is the same fallback `stamp` makes, so the two still agree with each other:
/// what they agree on there is that neither knows.
fn parse_date(raw: &str) -> Option<u64> {
    // An exact instant is independent of the machine's timezone. Natural
    // rolling intervals can display this DSL and remain identical when edited
    // or saved, rather than rounding a creation time to a calendar day.
    if let Some(seconds) = raw.strip_prefix('@') {
        if seconds.is_empty() || !seconds.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        return seconds
            .parse::<u64>()
            .ok()
            .filter(|&value| value != u64::MAX);
    }
    let mut parts = raw.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    // A day that exists, not merely a plausible number. `1..=31` accepted
    // `2026-02-30`, and the arithmetic below turns that into the 2nd of March
    // without complaint — a boundary two days from where it was typed. The check
    // is `kosha_core::format`'s, which is also what the FAT and ISO readers use
    // on the dates they find on a disk.
    if parts.next().is_some() || !kosha_core::format::is_a_date(year, month, day) {
        return None;
    }

    if let Some(local) = kosha_core::format::local_midnight(year, month, day) {
        return Some(local);
    }
    let days = kosha_core::format::days_from_civil(year, month, day);
    u64::try_from(days.checked_mul(86_400)?).ok()
}

/// Resolve a word to a field, accepting every alias the language does.
pub fn field_for_word(word: &str) -> Option<Field> {
    field_from_word(&word.to_ascii_lowercase())
}

/// Resolve a word to a flag, accepting every alias the language does.
pub fn flag_for_word(word: &str) -> Option<Flag> {
    flag_from_word(&word.to_ascii_lowercase())
}

fn field_from_word(lower: &str) -> Option<Field> {
    Some(match lower {
        "name" => Field::Name,
        "path" => Field::Path,
        "ext" | "extension" => Field::Ext,
        "size" => Field::Size,
        "apparent" | "logical" => Field::Apparent,
        "items" | "files" => Field::Items,
        "children" => Field::Children,
        "depth" => Field::Depth,
        "age" => Field::Age,
        "modified" | "mtime" => Field::Modified,
        "created" | "birth" | "birthtime" => Field::Created,
        "wasted" | "slack" => Field::Wasted,
        // `user` and `account` because "owner" is the filesystem's word for it
        // and not necessarily the one somebody reaches for.
        "owner" | "user" | "account" => Field::Owner,
        _ => return None,
    })
}

fn flag_from_word(lower: &str) -> Option<Flag> {
    Some(match lower {
        "dir" | "dirs" | "directory" | "folder" => Flag::Dir,
        "file" | "files_only" => Flag::File,
        "link" | "symlink" => Flag::Link,
        "sparse" | "compressed" => Flag::Sparse,
        "partial" => Flag::Partial,
        "hardlink" | "duplicate_inode" => Flag::Hardlink,
        "unreadable" => Flag::Unreadable,
        "empty" => Flag::Empty,
        _ => return None,
    })
}

/// Suggest the closest field name, so a typo is one glance from fixed.
fn nearest_field(word: &str) -> String {
    const CANDIDATES: &[&str] = &[
        "name", "path", "ext", "size", "apparent", "items", "children", "depth", "age", "modified",
        "created", "wasted", "owner",
    ];
    let best = CANDIDATES
        .iter()
        .map(|candidate| (*candidate, crate::help::edit_distance(word, candidate)))
        .filter(|(_, distance)| *distance <= 2)
        .min_by_key(|(_, distance)| *distance);

    match best {
        Some((candidate, _)) => format!("did you mean `{candidate}`?"),
        None => format!("known fields: {}", CANDIDATES.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_carry_matches_integer_ratios_without_fraction_length_overflow() {
        for scale in [1u64, 60, 1024, 1 << 40] {
            for fraction in 0..1000u128 {
                let amount = format!("3.{fraction:03}");
                let expected = ((3000 + fraction) * u128::from(scale) / 1000) as u64;
                assert_eq!(
                    scaled_decimal(&amount, scale),
                    Some(expected),
                    "{amount} * {scale}"
                );
            }
        }
        assert_eq!(
            scaled_decimal(&format!("0.{}", "9".repeat(4096)), 1024),
            Some(1023)
        );
        assert_eq!(
            scaled_decimal("18446744073709551615.9999", 1),
            Some(u64::MAX)
        );
        assert_eq!(scaled_decimal("18446744073709551615.9999", 1024), None);
    }

    /// Every accepted size suffix, against the multiplier it claims.
    ///
    /// These were reachable from the query language and pinned by nothing: an
    /// audit of the literals the parser accepts against the literals any test
    /// mentions found `gb`, `gib`, `kib`, `tb` and `tib` in the first column
    /// and not the second. A wrong multiplier here does not fail loudly — it
    /// returns a plausible set of rows that is the wrong set of rows.
    #[test]
    fn every_size_suffix_scales_by_what_it_says() {
        const K: u64 = 1024;
        for (input, expected) in [
            ("0", 0),
            ("512", 512),
            ("512b", 512),
            ("1k", K),
            ("1kb", K),
            ("1kib", K),
            ("1m", K * K),
            ("1mb", K * K),
            ("1mib", K * K),
            ("1g", K * K * K),
            ("1gb", K * K * K),
            ("1gib", K * K * K),
            ("1t", K * K * K * K),
            ("1tb", K * K * K * K),
            ("1tib", K * K * K * K),
            // Fractions and separators, which the interface encourages.
            ("1.5gb", K * K * K + K * K * K / 2),
            ("1_000", 1000),
            ("2.5k", 2560),
            // Case is not significant: the box is typed in, not generated.
            ("1GiB", K * K * K),
        ] {
            assert_eq!(parse_size(input), Some(expected), "`{input}`");
        }
    }

    #[test]
    fn a_size_that_is_not_one_is_refused_rather_than_guessed() {
        // `kbps` is the plausible typo; a partial match would read it as `k`.
        for input in ["", "mb", "1x", "1kbps", "-5mb", "1..5m", "1 2 3", "1e9"] {
            assert_eq!(parse_size(input), None, "`{input}` should not parse");
        }
    }

    /// Every accepted duration suffix, in seconds.
    #[test]
    fn every_duration_suffix_is_the_span_it_names() {
        const DAY: u64 = 86_400;
        for (input, expected) in [
            ("1s", 1),
            ("1sec", 1),
            ("1min", 60),
            ("1h", 3_600),
            ("1hr", 3_600),
            ("1hour", 3_600),
            ("2hours", 7_200),
            ("1d", DAY),
            ("1day", DAY),
            ("3days", 3 * DAY),
            ("1w", 7 * DAY),
            ("1week", 7 * DAY),
            ("2weeks", 14 * DAY),
            ("1mo", 30 * DAY),
            ("1month", 30 * DAY),
            ("6months", 180 * DAY),
            ("1y", 365 * DAY),
            ("1yr", 365 * DAY),
            ("1year", 365 * DAY),
            ("2years", 730 * DAY),
            // A bare number of days is what people mean by an age.
            ("30", 30 * DAY),
        ] {
            assert_eq!(parse_duration(input), Some(expected), "`{input}`");
        }
    }

    /// The ambiguity the language refuses to resolve on the user's behalf.
    #[test]
    fn a_bare_m_in_a_duration_is_refused_as_ambiguous() {
        assert_eq!(parse_duration("6m"), None);
        // And both unambiguous spellings work, so the refusal is not a gap.
        assert_eq!(parse_duration("6mo"), Some(180 * 86_400));
        assert_eq!(parse_duration("6min"), Some(360));
    }

    #[test]
    fn a_duration_that_is_not_one_is_refused() {
        for input in ["", "d", "1x", "-3d", "1..5y", "1 2 3"] {
            assert_eq!(parse_duration(input), None, "`{input}` should not parse");
        }
    }

    /// Every spelling the language accepts for a field, and nothing else.
    ///
    /// The aliases exist so somebody can write what they already have in their
    /// head — `logical` and `apparent`, `slack` and `wasted` — and an alias
    /// that silently resolves to the *wrong* field returns a plausible set of
    /// rows rather than an error. `logical` and `slack` were accepted by the
    /// parser and named in no test.
    #[test]
    fn every_field_spelling_resolves_to_one_field() {
        for (word, field) in [
            ("name", Field::Name),
            ("path", Field::Path),
            ("ext", Field::Ext),
            ("extension", Field::Ext),
            ("size", Field::Size),
            ("apparent", Field::Apparent),
            ("logical", Field::Apparent),
            ("items", Field::Items),
            ("files", Field::Items),
            ("children", Field::Children),
            ("depth", Field::Depth),
            ("age", Field::Age),
            ("modified", Field::Modified),
            ("mtime", Field::Modified),
            ("wasted", Field::Wasted),
            ("slack", Field::Wasted),
            ("owner", Field::Owner),
            ("user", Field::Owner),
            ("account", Field::Owner),
        ] {
            assert_eq!(field_from_word(word), Some(field), "`{word}`");
        }

        // A flag is not a field, and vice versa: they are separate tables and
        // a word that fell into both would parse as whichever was tried first.
        for word in ["dir", "sparse", "hardlink", "empty", "unreadable"] {
            assert_eq!(field_from_word(word), None, "`{word}` is a flag");
        }
        assert_eq!(field_from_word("sizes"), None, "not a field");
    }

    #[test]
    fn every_flag_spelling_resolves_to_one_flag() {
        for (word, flag) in [
            ("dir", Flag::Dir),
            ("dirs", Flag::Dir),
            ("directory", Flag::Dir),
            ("folder", Flag::Dir),
            ("file", Flag::File),
            ("files_only", Flag::File),
            ("link", Flag::Link),
            ("symlink", Flag::Link),
            ("sparse", Flag::Sparse),
            ("compressed", Flag::Sparse),
            ("partial", Flag::Partial),
            ("hardlink", Flag::Hardlink),
            ("duplicate_inode", Flag::Hardlink),
            ("unreadable", Flag::Unreadable),
            ("empty", Flag::Empty),
        ] {
            assert_eq!(flag_from_word(word), Some(flag), "`{word}`");
        }

        for word in ["name", "size", "age", "folders", "links"] {
            assert_eq!(flag_from_word(word), None, "`{word}` is not a flag");
        }
    }

    /// The suffix is trimmed, so a space between the number and its unit is
    /// accepted. The tokenizer splits on whitespace long before this sees the
    /// text, so it only shows up in a quoted value — where taking it is
    /// friendlier than refusing it.
    #[test]
    fn a_space_before_the_unit_is_tolerated() {
        assert_eq!(parse_duration("1 day"), Some(86_400));
        assert_eq!(parse_size("5 mb"), Some(5 * 1024 * 1024));
    }

    /// A day that does not exist is not a date.
    ///
    /// `1..=31` accepted `2026-02-30`, and `days_from_civil` turned it into the
    /// 2nd of March: a boundary two days from where it was typed, with no
    /// complaint and nothing on screen to show it had moved. `calendar` had the
    /// right check all along, for the dates it reads off a FAT or ISO volume,
    /// with the argument written above it — "a 31st of February is either damage
    /// or a formatter that did not care". A person typing one deserves the same
    /// answer.
    #[test]
    fn a_day_that_does_not_exist_is_refused_rather_than_moved() {
        for spelling in [
            "2026-02-30",
            "2026-02-29",
            "2023-02-29",
            "2100-02-29",
            "2026-04-31",
            "2026-13-01",
            "2026-00-10",
            "2026-01-00",
        ] {
            assert!(
                parse_date(spelling).is_none(),
                "`{spelling}` parsed, and as {:?}",
                parse_date(spelling)
            );
        }

        // The ones that do exist, including the leap days the rules turn on.
        for spelling in ["2026-02-28", "2024-02-29", "2000-02-29", "2026-12-31"] {
            assert!(parse_date(spelling).is_some(), "`{spelling}` was refused");
        }

        // And the specific slip: it used to come back as the day after February.
        assert_ne!(parse_date("2026-02-30"), parse_date("2026-03-02"));
    }

    /// A date in a filter has to mean the same day the program shows.
    ///
    /// `format::stamp` is what the table and the snapshot rail print, and it
    /// prints local time — so `modified < 2026-03-01` has to mean local midnight,
    /// or a row displayed as `28 Feb 23:30` falls on the wrong side of a boundary
    /// the user typed in the only spelling the language accepts.
    ///
    /// Vacuous where the machine runs on UTC, which is most build servers and is
    /// why this went unnoticed; it fails on any machine with an offset.
    #[test]
    fn a_date_means_midnight_where_the_person_typing_it_is() {
        let at = parse_date("2026-03-01").expect("a date parses");
        // A `now` well after the instant, so `stamp` prints the day and not just
        // a clock — it shortens to seconds for today.
        let now = at + 40 * 86_400;

        let shown = kosha_core::format::stamp(at, now);
        let before = kosha_core::format::stamp(at - 1, now);
        assert!(
            shown.starts_with("1 Mar"),
            "the boundary is shown as {shown}, not as the day that was asked for"
        );
        assert!(
            before.starts_with("28 Feb"),
            "the second before 1 March is shown as {before}, so the filter and \
             the table disagree about which day a row is on"
        );
    }
}
