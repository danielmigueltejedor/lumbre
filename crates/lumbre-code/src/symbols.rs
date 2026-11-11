//! Symbol search by pattern.
//!
//! These are line-based heuristics, not a parser: they find definitions written the usual way
//! and miss unusual formatting. A syntax-tree index is planned; until then this is honest about
//! being approximate and never claims a symbol does not exist.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    /// `fn`, `struct`, `class`, … as written in the source language.
    pub kind: String,
    pub path: PathBuf,
    /// 1-based.
    pub line: usize,
}

pub const SUPPORTED_EXTENSIONS: &[&str] =
    &["rs", "py", "js", "jsx", "ts", "tsx", "go", "c", "h", "cpp", "hpp", "cc", "java", "kt"];

/// A pattern with a named `name` group and either a named `kind` group or a fixed kind.
struct Rule {
    regex: Regex,
    kind: Option<&'static str>,
}

fn rules(extension: &str) -> &'static [Rule] {
    static TABLES: OnceLock<[Vec<Rule>; 6]> = OnceLock::new();
    let tables = TABLES.get_or_init(|| {
        let rule = |pattern: &str, kind| Rule {
            regex: Regex::new(pattern).unwrap_or_else(|error| panic!("built-in symbol pattern is invalid: {error}")),
            kind,
        };
        [
            // Rust
            vec![
                rule(r#"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:(?:async|unsafe|const|extern\s+"[^"]*")\s+)*(?P<kind>fn)\s+(?P<name>\w+)"#, None),
                rule(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?P<kind>struct|enum|trait|mod|type|union|const|static)\s+(?:mut\s+)?(?P<name>\w+)", None),
                rule(r"^\s*macro_rules!\s+(?P<name>\w+)", Some("macro")),
            ],
            // Python
            vec![rule(r"^\s*(?:async\s+)?(?P<kind>def|class)\s+(?P<name>\w+)", None)],
            // JavaScript / TypeScript
            vec![
                rule(r"^\s*(?:export\s+)?(?:default\s+)?(?:declare\s+)?(?:abstract\s+)?(?:async\s+)?(?P<kind>function|class|interface|type|enum)\s*\*?\s*(?P<name>[A-Za-z_$][\w$]*)", None),
                rule(r"^\s*(?:export\s+)?(?:const|let|var)\s+(?P<name>[A-Za-z_$][\w$]*)\s*(?::[^=]+)?=\s*(?:async\s*)?(?:\([^)]*\)|[A-Za-z_$][\w$]*)\s*(?::[^=]+)?=>", Some("function")),
            ],
            // Go
            vec![
                rule(r"^func\s+(?:\([^)]*\)\s*)?(?P<name>\w+)", Some("func")),
                rule(r"^type\s+(?P<name>\w+)\s+(?P<kind>struct|interface)", None),
            ],
            // C / C++ (types only; function definitions need a parser)
            vec![rule(r"^\s*(?:typedef\s+)?(?P<kind>class|struct|union|enum|namespace)\s+(?:class\s+)?(?P<name>[A-Za-z_]\w*)\s*(?:[:{]|$)", None)],
            // Java / Kotlin
            vec![
                rule(r"^\s*(?:(?:public|private|protected|internal|abstract|final|static|sealed|data|open)\s+)*(?P<kind>class|interface|enum|record|object)\s+(?P<name>\w+)", None),
                rule(r"^\s*(?:(?:public|private|protected|internal|override|suspend|inline|open)\s+)*(?P<kind>fun)\s+(?:<[^>]*>\s*)?(?:\w+(?:<[^>]*>)?\??\.)?(?P<name>\w+)", None),
            ],
        ]
    });
    let index = match extension {
        "rs" => 0,
        "py" => 1,
        "js" | "jsx" | "ts" | "tsx" => 2,
        "go" => 3,
        "c" | "h" | "cpp" | "hpp" | "cc" => 4,
        "java" | "kt" => 5,
        _ => return &[],
    };
    &tables[index]
}

pub(crate) fn extract(path: &Path, extension: &str, text: &str) -> Vec<Symbol> {
    let rules = rules(extension);
    let mut symbols = Vec::new();
    for (index, line) in text.lines().enumerate() {
        for rule in rules {
            let Some(captures) = rule.regex.captures(line) else { continue };
            let Some(name) = captures.name("name") else { continue };
            let kind =
                rule.kind.or_else(|| captures.name("kind").map(|kind| kind.as_str())).unwrap_or("symbol");
            symbols.push(Symbol {
                name: name.as_str().to_owned(),
                kind: kind.to_owned(),
                path: path.to_owned(),
                line: index + 1,
            });
            break;
        }
    }
    symbols
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(extension: &str, text: &str) -> Vec<(String, String, usize)> {
        extract(Path::new("x"), extension, text)
            .into_iter()
            .map(|symbol| (symbol.kind, symbol.name, symbol.line))
            .collect()
    }

    fn expect(extension: &str, text: &str, expected: &[(&str, &str, usize)]) {
        let expected: Vec<_> = expected
            .iter()
            .map(|(kind, name, line)| ((*kind).to_owned(), (*name).to_owned(), *line))
            .collect();
        assert_eq!(names(extension, text), expected, "{extension}");
    }

    #[test]
    fn rust() {
        let text = "pub(crate) async fn load() {}\nstruct Engine;\n    pub const fn new() -> Self {}\npub static mut COUNT: u32 = 0;\nimpl Engine {}\nmacro_rules! bail {}\nlet x = fn_like();\n";
        expect(
            "rs",
            text,
            &[
                ("fn", "load", 1),
                ("struct", "Engine", 2),
                ("fn", "new", 3),
                ("static", "COUNT", 4),
                ("macro", "bail", 6),
            ],
        );
    }

    #[test]
    fn python_and_go() {
        expect(
            "py",
            "class A:\n    async def run(self):\n        x = 1\n",
            &[("class", "A", 1), ("def", "run", 2)],
        );
        expect(
            "go",
            "func (s *Server) Serve() {}\nfunc main() {}\ntype Config struct {\n",
            &[("func", "Serve", 1), ("func", "main", 2), ("struct", "Config", 3)],
        );
    }

    #[test]
    fn javascript_and_typescript() {
        let text = "export default async function* stream() {}\nexport const add = (a: number, b: number): number => a + b\ninterface Props {}\nconst notAFunction = 3\nexport type Id = string\n";
        expect(
            "ts",
            text,
            &[
                ("function", "stream", 1),
                ("function", "add", 2),
                ("interface", "Props", 3),
                ("type", "Id", 5),
            ],
        );
    }

    #[test]
    fn c_family_and_jvm() {
        expect(
            "hpp",
            "namespace vortia {\nclass Mesh : public Base {\nenum class Kind {\nstruct Mesh* next;\n",
            &[("namespace", "vortia", 1), ("class", "Mesh", 2), ("enum", "Kind", 3)],
        );
        expect("kt", "data class User(val id: Int)\n    override suspend fun load(): User {}\nfun <T> List<T>.second(): T {}\n", &[("class", "User", 1), ("fun", "load", 2), ("fun", "second", 3)]);
    }

    #[test]
    fn unknown_languages_yield_nothing() {
        assert!(names("md", "# fn main()").is_empty());
    }
}
