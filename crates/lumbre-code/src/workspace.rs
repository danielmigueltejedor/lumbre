use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use ignore::WalkBuilder;
use regex::Regex;

use crate::symbols::{self, Symbol};

/// Files larger than this are skipped by content searches: they are data, not source.
const MAX_SEARCHED_BYTES: u64 = 2 << 20;

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("{0} is not a directory")]
    NotADirectory(PathBuf),
    #[error("`{0}` is outside the workspace")]
    Outside(String),
    #[error("invalid pattern: {0}")]
    Pattern(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// A source tree rooted at one directory. Every path it hands out or accepts is inside it.
#[derive(Clone, Debug)]
pub struct Workspace {
    root: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// Path relative to the workspace root.
    pub path: PathBuf,
    /// 1-based.
    pub line: usize,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overview {
    pub files: usize,
    pub lines: usize,
    /// Extension → (files, lines), for text files.
    pub by_extension: BTreeMap<String, (usize, usize)>,
    /// Build and project manifests found at any depth (`Cargo.toml`, `package.json`, …).
    pub manifests: Vec<PathBuf>,
    pub is_git: bool,
}

const MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
    "CMakeLists.txt",
    "Makefile",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "meson.build",
    "flake.nix",
];

impl Workspace {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, WorkspaceError> {
        let root = root.as_ref();
        if !root.is_dir() {
            return Err(WorkspaceError::NotADirectory(root.to_owned()));
        }
        Ok(Self { root: root.canonicalize()? })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Turns a path from a diff or a command line into a path inside the workspace.
    ///
    /// Absolute paths and anything that climbs out with `..` are refused, and so is a path whose
    /// existing parent directory resolves (through a symlink) to somewhere else.
    pub fn resolve(&self, relative: impl AsRef<Path>) -> Result<PathBuf, WorkspaceError> {
        let relative = relative.as_ref();
        let outside = || WorkspaceError::Outside(relative.display().to_string());
        let mut clean = PathBuf::new();
        for component in relative.components() {
            match component {
                Component::Normal(part) => clean.push(part),
                Component::CurDir => {}
                Component::ParentDir if clean.pop() => {}
                _ => return Err(outside()),
            }
        }
        if clean.as_os_str().is_empty() {
            return Err(outside());
        }
        let full = self.root.join(&clean);
        // The deepest ancestor that exists must still be inside the root once symlinks resolve.
        let mut existing = full.as_path();
        while !existing.exists() {
            existing = existing.parent().ok_or_else(outside)?;
        }
        if !existing.canonicalize()?.starts_with(&self.root) {
            return Err(outside());
        }
        Ok(full)
    }

    /// Every file a developer would consider part of the project, relative to the root, sorted.
    /// Hidden files, `.git` and anything matched by ignore files are left out.
    pub fn files(&self) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = WalkBuilder::new(&self.root)
            .build()
            .flatten()
            .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
            .filter_map(|entry| entry.path().strip_prefix(&self.root).ok().map(Path::to_owned))
            .collect();
        files.sort();
        files
    }

    fn read_text(&self, relative: &Path) -> Option<String> {
        let path = self.root.join(relative);
        if fs::metadata(&path).ok()?.len() > MAX_SEARCHED_BYTES {
            return None;
        }
        // Not UTF-8 means binary for our purposes.
        String::from_utf8(fs::read(path).ok()?).ok()
    }

    pub fn overview(&self) -> Overview {
        let mut overview = Overview { is_git: self.root.join(".git").exists(), ..Default::default() };
        for file in self.files() {
            overview.files += 1;
            if file.file_name().and_then(|name| name.to_str()).is_some_and(|name| MANIFESTS.contains(&name)) {
                overview.manifests.push(file.clone());
            }
            let Some(text) = self.read_text(&file) else { continue };
            let lines = text.lines().count();
            overview.lines += lines;
            let extension = file.extension().and_then(|ext| ext.to_str()).unwrap_or("(none)").to_lowercase();
            let entry = overview.by_extension.entry(extension).or_default();
            entry.0 += 1;
            entry.1 += lines;
        }
        overview
    }

    /// Files whose relative path contains every word of the query (case-insensitive).
    pub fn find_files(&self, query: &str) -> Vec<PathBuf> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        self.files()
            .into_iter()
            .filter(|file| {
                let path = file.to_string_lossy().to_lowercase();
                !words.is_empty() && words.iter().all(|word| path.contains(word))
            })
            .collect()
    }

    /// Lines matching a regular expression, up to `limit`.
    pub fn grep(&self, pattern: &str, limit: usize) -> Result<Vec<Match>, WorkspaceError> {
        let regex = Regex::new(pattern).map_err(|error| WorkspaceError::Pattern(error.to_string()))?;
        let mut matches = Vec::new();
        'files: for file in self.files() {
            let Some(text) = self.read_text(&file) else { continue };
            for (index, line) in text.lines().enumerate().filter(|(_, line)| regex.is_match(line)) {
                if matches.len() == limit {
                    break 'files;
                }
                matches.push(Match { path: file.clone(), line: index + 1, text: line.trim_end().to_owned() });
            }
        }
        Ok(matches)
    }

    /// Definitions whose name contains `query` (case-insensitive); an empty query lists all.
    /// Exact name matches come first.
    pub fn symbols(&self, query: &str) -> Vec<Symbol> {
        let needle = query.to_lowercase();
        let mut found = Vec::new();
        for file in self.files() {
            let Some(extension) = file.extension().and_then(|ext| ext.to_str()) else { continue };
            if !symbols::SUPPORTED_EXTENSIONS.contains(&extension) {
                continue;
            }
            let Some(text) = self.read_text(&file) else { continue };
            found.extend(
                symbols::extract(&file, extension, &text)
                    .into_iter()
                    .filter(|symbol| symbol.name.to_lowercase().contains(&needle)),
            );
        }
        found.sort_by_key(|symbol| (symbol.name.to_lowercase() != needle, symbol.path.clone(), symbol.line));
        found
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn sandbox(name: &str) -> Workspace {
        let dir = std::env::temp_dir().join(format!("lumbre-code-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("target/debug")).unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        fs::write(dir.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {\n    println!(\"{}\", add(2, 2));\n}\n\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n").unwrap();
        fs::write(dir.join("src/util.py"), "class Parser:\n    def parse(self):\n        pass\n").unwrap();
        fs::write(dir.join("target/debug/junk.rs"), "fn ignored() {}\n").unwrap();
        fs::write(dir.join("logo.bin"), [0u8, 159, 146, 150]).unwrap();
        Workspace::open(dir).unwrap()
    }

    pub(crate) fn cleanup(workspace: Workspace) {
        fs::remove_dir_all(workspace.root()).unwrap();
    }

    #[test]
    fn files_respect_gitignore_and_skip_hidden_entries() {
        let workspace = sandbox("files");
        let files: Vec<_> =
            workspace.files().iter().map(|file| file.to_string_lossy().into_owned()).collect();
        assert_eq!(files, ["Cargo.toml", "logo.bin", "src/main.rs", "src/util.py"]);
        assert_eq!(workspace.find_files("SRC main"), [PathBuf::from("src/main.rs")]);
        assert!(workspace.find_files("").is_empty());
        cleanup(workspace);
    }

    #[test]
    fn overview_counts_text_and_finds_manifests() {
        let workspace = sandbox("overview");
        let overview = workspace.overview();
        assert_eq!((overview.files, overview.is_git), (4, true));
        assert_eq!(overview.by_extension["rs"], (1, 7));
        assert_eq!(overview.by_extension["py"], (1, 3));
        assert!(!overview.by_extension.contains_key("bin"));
        assert_eq!(overview.manifests, [PathBuf::from("Cargo.toml")]);
        cleanup(workspace);
    }

    #[test]
    fn grep_reports_lines_and_honours_the_limit() {
        let workspace = sandbox("grep");
        let matches = workspace.grep(r"fn \w+\(", 10).unwrap();
        let lines: Vec<_> = matches.iter().map(|m| (m.path.to_string_lossy().into_owned(), m.line)).collect();
        assert_eq!(lines, [("src/main.rs".to_owned(), 1), ("src/main.rs".to_owned(), 5)]);
        assert_eq!(workspace.grep("a", 1).unwrap().len(), 1);
        assert!(matches!(workspace.grep("(", 1), Err(WorkspaceError::Pattern(_))));
        cleanup(workspace);
    }

    #[test]
    fn paths_cannot_leave_the_workspace() {
        let workspace = sandbox("resolve");
        assert_eq!(workspace.resolve("src/../src/new.rs").unwrap(), workspace.root().join("src/new.rs"));
        for bad in ["../x", "/etc/passwd", "src/../../x", "", "."] {
            assert!(matches!(workspace.resolve(bad), Err(WorkspaceError::Outside(_))), "{bad}");
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(std::env::temp_dir(), workspace.root().join("escape")).unwrap();
            assert!(matches!(workspace.resolve("escape/x.txt"), Err(WorkspaceError::Outside(_))));
        }
        cleanup(workspace);
    }
}
