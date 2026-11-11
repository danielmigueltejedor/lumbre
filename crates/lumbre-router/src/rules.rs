//! Stage 1: deterministic rules for operations that never need a model.
//!
//! Every rule works on the normalised text (see [`crate::normalize`]) in Spanish and English.
//! Rules are deliberately narrow: a false positive sends a real question to the wrong tool, a
//! false negative only costs a model call.

use serde_json::json;

use crate::{tool, Route};

/// Rules only fire on short requests; a paragraph that happens to contain "what time is it" is a
/// question for a model.
const MAX_WORDS: usize = 9;

const TIME: &[&str] = &[
    "que hora es",
    "que hora tenemos",
    "dime la hora",
    "hora actual",
    "what time is it",
    "whats the time",
    "what is the time",
    "current time",
    "tell me the time",
];

const DATE: &[&str] = &[
    "que dia es",
    "que fecha es",
    "fecha de hoy",
    "a que dia estamos",
    "a que estamos hoy",
    "what day is it",
    "what is the date",
    "whats the date",
    "todays date",
    "what is todays date",
    "current date",
];

const CALC_PREFIXES: &[&str] =
    &["cuanto es ", "cuanto son ", "cuanto da ", "calcula ", "what is ", "whats ", "calculate ", "compute "];

const FIND_VERBS: &[&str] = &[
    "buscame ",
    "busca ",
    "encuentra ",
    "localiza ",
    "donde esta ",
    "search for ",
    "find ",
    "locate ",
    "where is ",
];
const ARTICLES: &[&str] = &["el ", "la ", "un ", "una ", "mi ", "the ", "a ", "my "];
const FILE_NOUNS: &[&str] = &["archivo ", "fichero ", "documento ", "file ", "document "];
const CALLED: &[&str] = &["llamado ", "que se llama ", "de nombre ", "named ", "called "];

pub(crate) fn apply(original: &str, normalized: &str) -> Option<(Route, String)> {
    if normalized.split_whitespace().count() > MAX_WORDS {
        return None;
    }
    if TIME.iter().any(|phrase| normalized.contains(phrase)) {
        return Some(clock("time"));
    }
    if DATE.iter().any(|phrase| normalized.contains(phrase)) {
        return Some(clock("date"));
    }
    if let Some(expr) = arithmetic(normalized) {
        let route = Route::Tool { name: tool::CALC.into(), args: json!({ "expr": expr }) };
        return Some((route, "rule: arithmetic expression".into()));
    }
    if let Some(query) = file_query(original, normalized) {
        let route = Route::Tool { name: tool::FIND_FILES.into(), args: json!({ "query": query }) };
        return Some((route, "rule: find a local file by name".into()));
    }
    None
}

fn clock(what: &str) -> (Route, String) {
    let route = Route::Tool { name: tool::CLOCK.into(), args: json!({ "what": what }) };
    (route, format!("rule: asks for the current {what}"))
}

/// The expression, if the whole request is one: only digits, operators and brackets, with at
/// least one digit and one operator.
fn arithmetic(normalized: &str) -> Option<String> {
    let body = CALC_PREFIXES.iter().find_map(|prefix| normalized.strip_prefix(prefix)).unwrap_or(normalized);
    let body = body.trim();
    // "3 x 4" and "3×4" mean multiplication; a bare 'x' anywhere else is not arithmetic.
    let body = body.replace(['×', '·'], "*").replace('÷', "/").replace(" x ", " * ");
    let allowed = |ch: char| ch.is_ascii_digit() || " .,+-*/^%()".contains(ch);
    let has_digit = body.chars().any(|ch| ch.is_ascii_digit());
    let has_operator = body.chars().skip(1).any(|ch| "+-*/^%".contains(ch));
    (body.chars().all(allowed) && has_digit && has_operator).then_some(body)
}

/// The file name asked for, taken from the original text so its case and accents survive.
fn file_query(original: &str, normalized: &str) -> Option<String> {
    let strip = |text: &'_ str, options: &[&str]| -> Option<usize> {
        options.iter().find(|option| text.starts_with(**option)).map(|option| option.len())
    };
    let mut at = strip(normalized, FIND_VERBS)?;
    at += strip(&normalized[at..], ARTICLES).unwrap_or(0);
    at += strip(&normalized[at..], FILE_NOUNS)?;
    at += strip(&normalized[at..], CALLED).unwrap_or(0);
    let words_before = normalized[..at].split_whitespace().count();
    // Normalisation never merges or splits words, so the same word index is valid in the original.
    let query: Vec<&str> = original.split_whitespace().skip(words_before).collect();
    let query = query.join(" ");
    let query = query.trim_matches(|ch: char| "¿?¡!.,;:\"'«»".contains(ch)).to_owned();
    (!query.is_empty()).then_some(query)
}
