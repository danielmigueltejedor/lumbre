//! Stage 2: a keyword scorer. Crude on purpose — it must cost nothing and be explainable; the
//! cases it cannot decide are exactly the ones handed to the resolver.

use crate::Intent;

/// Strong signs of source code: syntax that prose does not contain.
const CODE_SYNTAX: &[&str] = &[
    "```",
    "fn ",
    "def ",
    "#include",
    "import ",
    "=>",
    "();",
    "){",
    ") {",
    "</",
    "::",
    "println!",
    "console.log",
    "select * from",
    "null pointer",
    "stack trace",
    "segfault",
    "traceback",
];
/// File extensions as they appear in a sentence.
const CODE_EXTENSIONS: &[&str] = &[
    ".rs", ".py", ".js", ".ts", ".tsx", ".go", ".c", ".cpp", ".h", ".java", ".kt", ".sql", ".sh", ".toml",
    ".json",
];
/// Words about programming (normalised, Spanish and English).
const CODE_WORDS: &[&str] = &[
    "codigo",
    "code",
    "bug",
    "compila",
    "compile",
    "compilar",
    "compiles",
    "funcion",
    "function",
    "refactor",
    "refactoriza",
    "script",
    "clase",
    "class",
    "rust",
    "python",
    "javascript",
    "typescript",
    "sql",
    "regex",
    "api",
    "test",
    "tests",
    "linter",
    "git",
    "commit",
    "variable",
    "bucle",
    "loop",
    "struct",
    "array",
    "programa",
    "program",
    "depura",
    "debug",
    "excepcion",
    "exception",
];
/// Requests for explanation, comparison or planning.
const REASONING_PHRASES: &[&str] = &[
    "por que",
    "paso a paso",
    "step by step",
    "pros y contras",
    "pros and cons",
    "ventajas e inconvenientes",
    "trade-off",
    "tradeoff",
];
const REASONING_WORDS: &[&str] = &[
    "why",
    "explica",
    "explicame",
    "explain",
    "demuestra",
    "prove",
    "analiza",
    "analyze",
    "analyse",
    "compara",
    "compare",
    "razona",
    "reason",
    "planifica",
    "plan",
    "estrategia",
    "strategy",
    "evalua",
    "evaluate",
    "justifica",
    "justify",
    "deduce",
    "derive",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scores {
    pub code: u32,
    pub reasoning: u32,
}

impl Scores {
    /// The intent and a confidence, or `None` when the scores are too close to call.
    pub fn verdict(self) -> Option<(Intent, f32)> {
        let Scores { code, reasoning } = self;
        let total = (code + reasoning) as f32;
        match (code, reasoning) {
            (0, 0) => Some((Intent::Chat, 0.7)),
            (code, reasoning) if code >= 2 && code > reasoning => {
                Some((Intent::Code, code as f32 / (total + 1.0)))
            }
            (code, reasoning) if reasoning >= 2 && reasoning > code => {
                Some((Intent::Reasoning, reasoning as f32 / (total + 1.0)))
            }
            // A single weak signal, or a tie.
            _ => None,
        }
    }
}

pub fn classify(original: &str, normalized: &str) -> Scores {
    let lower = original.to_lowercase();
    let words: Vec<&str> = normalized
        .split(|ch: char| !ch.is_alphanumeric() && ch != '-')
        .filter(|word| !word.is_empty())
        .collect();
    let mut scores = Scores::default();

    scores.code += 2 * CODE_SYNTAX.iter().filter(|needle| lower.contains(**needle)).count() as u32;
    let has_extension = lower.split_whitespace().any(|word| {
        let word = word.trim_end_matches(|ch: char| !ch.is_alphanumeric());
        CODE_EXTENSIONS.iter().any(|ext| word.len() > ext.len() && word.ends_with(ext))
    });
    scores.code += 2 * u32::from(has_extension);
    scores.code += words.iter().filter(|word| CODE_WORDS.contains(word)).count() as u32;

    scores.reasoning +=
        2 * REASONING_PHRASES.iter().filter(|phrase| normalized.contains(**phrase)).count() as u32;
    scores.reasoning += words.iter().filter(|word| REASONING_WORDS.contains(word)).count() as u32;
    // Long prompts tend to need more than a quick answer.
    scores.reasoning += u32::from(words.len() > 60);
    scores
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize;

    fn scores(text: &str) -> Scores {
        classify(text, &normalize(text))
    }

    #[test]
    fn syntax_and_extensions_weigh_more_than_words() {
        assert_eq!(scores("hello there"), Scores::default());
        assert!(scores("what does `fn main() {}` do").code >= 2);
        assert_eq!(scores("open main.rs").code, 2);
        // "a.c" style false friends: an abbreviation ending in a dot is not an extension.
        assert_eq!(scores("see you at 5 p.m.").code, 0);
    }

    #[test]
    fn verdicts() {
        assert_eq!(Scores { code: 0, reasoning: 0 }.verdict().unwrap().0, Intent::Chat);
        assert_eq!(Scores { code: 3, reasoning: 1 }.verdict().unwrap().0, Intent::Code);
        assert_eq!(Scores { code: 0, reasoning: 2 }.verdict().unwrap().0, Intent::Reasoning);
        assert_eq!(Scores { code: 1, reasoning: 0 }.verdict(), None);
        assert_eq!(Scores { code: 2, reasoning: 2 }.verdict(), None);
    }
}
