//! `files.find`: look for files by name under the roots the user configured.
//!
//! It matches names only and never opens a file. Hidden files and anything ignored by a
//! `.gitignore` are skipped, the same way `ripgrep` does.

use std::path::PathBuf;

use ignore::WalkBuilder;
use serde_json::{json, Value};

use crate::{string_arg, Context, Language, Output, Permission, Tool, ToolError};

const MAX_RESULTS: usize = 20;
/// Upper bound on directory entries visited, so a huge home directory cannot stall a request.
const MAX_VISITED: usize = 200_000;

fn fold(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|ch| match ch {
            'á' => 'a',
            'é' => 'e',
            'í' => 'i',
            'ó' => 'o',
            'ú' | 'ü' => 'u',
            other => other,
        })
        .collect()
}

/// Files whose name contains every word of the query, best match first.
pub(crate) fn find(roots: &[PathBuf], query: &str) -> (Vec<PathBuf>, bool) {
    let query = fold(query);
    let words: Vec<&str> = query.split_whitespace().collect();
    let mut hits: Vec<(usize, PathBuf)> = Vec::new();
    let mut visited = 0;
    let mut truncated = false;
    'roots: for root in roots {
        for entry in WalkBuilder::new(root).build().flatten() {
            visited += 1;
            if visited > MAX_VISITED {
                truncated = true;
                break 'roots;
            }
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let name = fold(&entry.file_name().to_string_lossy());
            if !words.is_empty() && words.iter().all(|word| name.contains(word)) {
                // Shorter names that contain the whole query rank first.
                let exact = usize::from(!name.contains(&query));
                hits.push((exact * 10_000 + name.len(), entry.into_path()));
            }
        }
    }
    hits.sort();
    truncated |= hits.len() > MAX_RESULTS;
    hits.truncate(MAX_RESULTS);
    (hits.into_iter().map(|(_, path)| path).collect(), truncated)
}

pub(crate) struct FindFiles;

impl Tool for FindFiles {
    fn name(&self) -> &'static str {
        "files.find"
    }

    fn description(&self) -> &'static str {
        "Finds files by name under the configured folders. Reads names only, never contents."
    }

    fn permission(&self) -> Permission {
        Permission::ReadFiles
    }

    fn invoke(&self, args: &Value, context: &Context) -> Result<Output, ToolError> {
        let query = string_arg(args, "query")?;
        if context.roots.is_empty() {
            return Err(ToolError::Failed("no search folders are configured".into()));
        }
        let (paths, truncated) = find(&context.roots, query);
        let listing: Vec<String> = paths.iter().map(|path| path.display().to_string()).collect();
        let text = match (listing.is_empty(), context.language) {
            (true, Language::Es) => format!("No he encontrado ningún archivo que coincida con «{query}»."),
            (true, Language::En) => format!("No file matching \"{query}\" was found."),
            (false, Language::Es) => format!("Archivos que coinciden con «{query}»:\n{}", listing.join("\n")),
            (false, Language::En) => format!("Files matching \"{query}\":\n{}", listing.join("\n")),
        };
        Ok(Output { text, data: json!({ "query": query, "paths": listing, "truncated": truncated }) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn sandbox(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lumbre-tools-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("docs/2026")).unwrap();
        fs::write(dir.join("docs/2026/Presupuesto Marzo.pdf"), b"").unwrap();
        fs::write(dir.join("docs/presupuesto-borrador-largo.txt"), b"").unwrap();
        fs::write(dir.join("docs/notas.md"), b"").unwrap();
        fs::write(dir.join(".secreto-presupuesto"), b"").unwrap();
        dir
    }

    #[test]
    fn matches_every_word_ignoring_case_and_accents_and_skips_hidden_files() {
        let dir = sandbox("match");
        let (paths, truncated) = find(std::slice::from_ref(&dir), "presupuesto");
        let names: Vec<_> =
            paths.iter().map(|path| path.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["Presupuesto Marzo.pdf", "presupuesto-borrador-largo.txt"]);
        assert!(!truncated);
        assert_eq!(find(std::slice::from_ref(&dir), "marzo PRESUPUESTO").0.len(), 1);
        assert!(find(std::slice::from_ref(&dir), "factura").0.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_tool_only_looks_inside_its_roots() {
        let dir = sandbox("roots");
        let context = Context {
            granted: vec![Permission::ReadFiles],
            roots: vec![dir.join("docs/2026")],
            ..Default::default()
        };
        let output = FindFiles.invoke(&json!({"query": "notas"}), &context).unwrap();
        assert_eq!(output.data["paths"], json!([]));
        let no_roots = Context { granted: vec![Permission::ReadFiles], ..Default::default() };
        assert!(FindFiles.invoke(&json!({"query": "notas"}), &no_roots).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
