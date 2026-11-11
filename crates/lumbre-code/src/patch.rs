//! Changes as reviewable unified diffs.
//!
//! A diff is first turned into a [`Plan`] against the working tree: every file is located inside
//! the workspace, every hunk is applied in memory, and any mismatch fails the whole plan. Only a
//! complete plan can be written, so a diff is applied entirely or not at all.

use std::fs;
use std::path::PathBuf;

use crate::{Workspace, WorkspaceError};

#[derive(Debug, thiserror::Error)]
pub enum PatchError {
    #[error("the diff contains no file changes")]
    Empty,
    #[error("malformed diff near line {line}: {reason}")]
    Malformed { line: usize, reason: String },
    #[error("{0}")]
    Workspace(#[from] WorkspaceError),
    #[error("{path}: {reason}")]
    DoesNotApply { path: String, reason: String },
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Create,
    Modify,
    Delete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// Relative to the workspace root.
    pub path: PathBuf,
    pub kind: ChangeKind,
    pub added: usize,
    pub removed: usize,
    /// The file's full contents after the change; `None` for a deletion.
    new_contents: Option<String>,
}

/// A diff that has been checked against the working tree and is ready to write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub changes: Vec<Change>,
}

/// One file's part of a diff: its `---`/`+++` header and hunks, as text.
struct Section<'a> {
    old: &'a str,
    new: &'a str,
    text: String,
    added: usize,
    removed: usize,
}

/// `@@ -a,b +c,d @@` → `(b, d)`; an omitted count is 1.
fn hunk_lengths(header: &str) -> Option<(usize, usize)> {
    let mut parts = header.strip_prefix("@@ ")?.split(' ');
    let count = |range: &str, sign: char| -> Option<usize> {
        let range = range.strip_prefix(sign)?;
        range.split_once(',').map_or(Some(1), |(_, count)| count.parse().ok())
    };
    Some((count(parts.next()?, '-')?, count(parts.next()?, '+')?))
}

fn header_path(line: &str) -> &str {
    // `--- a/src/x.rs\t2026-01-01 ...` → `a/src/x.rs`
    line[4..].split('\t').next().unwrap_or("").trim()
}

/// Splits a (possibly multi-file, possibly `git diff`) patch into per-file sections, counting
/// hunk lines so that content which merely looks like a header is never mistaken for one.
fn sections(diff: &str) -> Result<Vec<Section<'_>>, PatchError> {
    let lines: Vec<&str> = diff.lines().collect();
    let malformed =
        |line: usize, reason: &str| PatchError::Malformed { line: line + 1, reason: reason.to_owned() };
    let mut sections = Vec::new();
    let mut at = 0;
    while at < lines.len() {
        let is_header =
            lines[at].starts_with("--- ") && lines.get(at + 1).is_some_and(|next| next.starts_with("+++ "));
        if !is_header {
            // Preamble: `diff --git`, `index`, mode lines, commit messages.
            at += 1;
            continue;
        }
        let mut section = Section {
            old: header_path(lines[at]),
            new: header_path(lines[at + 1]),
            text: format!("{}\n{}\n", lines[at], lines[at + 1]),
            added: 0,
            removed: 0,
        };
        at += 2;
        let mut hunks = 0;
        while at < lines.len() && lines[at].starts_with("@@ ") {
            let (mut old_left, mut new_left) =
                hunk_lengths(lines[at]).ok_or_else(|| malformed(at, "unreadable hunk header"))?;
            section.text.push_str(lines[at]);
            section.text.push('\n');
            at += 1;
            while old_left > 0 || new_left > 0 {
                let line = *lines
                    .get(at)
                    .ok_or_else(|| malformed(at, "the hunk is shorter than its header says"))?;
                match line.chars().next() {
                    Some('+') if new_left > 0 => {
                        new_left -= 1;
                        section.added += 1;
                    }
                    Some('-') if old_left > 0 => {
                        old_left -= 1;
                        section.removed += 1;
                    }
                    // An empty line is a context line whose single space was stripped.
                    Some(' ') | None if old_left > 0 && new_left > 0 => {
                        old_left -= 1;
                        new_left -= 1;
                    }
                    Some('\\') => {}
                    _ => return Err(malformed(at, "unexpected line inside a hunk")),
                }
                section.text.push_str(if line.is_empty() { " " } else { line });
                section.text.push('\n');
                at += 1;
            }
            if lines.get(at).is_some_and(|line| line.starts_with('\\')) {
                section.text.push_str(lines[at]);
                section.text.push('\n');
                at += 1;
            }
            hunks += 1;
        }
        if hunks == 0 {
            return Err(malformed(at, "a file header is not followed by a hunk"));
        }
        sections.push(section);
    }
    Ok(sections)
}

/// Removes the `a/` or `b/` prefix that `git diff` adds.
fn strip_prefix(path: &str) -> &str {
    path.strip_prefix("a/").or_else(|| path.strip_prefix("b/")).unwrap_or(path)
}

/// Checks a diff against the working tree without writing anything.
pub fn plan(workspace: &Workspace, diff: &str) -> Result<Plan, PatchError> {
    let mut changes: Vec<Change> = Vec::new();
    for section in sections(diff)? {
        let (target, kind) = match (section.old, section.new) {
            ("/dev/null", new) => (strip_prefix(new), ChangeKind::Create),
            (old, "/dev/null") => (strip_prefix(old), ChangeKind::Delete),
            (_, new) => (strip_prefix(new), ChangeKind::Modify),
        };
        let full = workspace.resolve(target)?;
        let relative =
            full.strip_prefix(workspace.root()).map_or_else(|_| PathBuf::from(target), PathBuf::from);
        let fail = |reason: String| PatchError::DoesNotApply { path: target.to_owned(), reason };
        if changes.iter().any(|change| change.path == relative) {
            return Err(fail("the diff changes this file more than once".into()));
        }
        let base = match (kind, full.exists()) {
            (ChangeKind::Create, true) => return Err(fail("the file already exists".into())),
            (ChangeKind::Create, false) => String::new(),
            (_, false) => return Err(fail("the file does not exist".into())),
            (_, true) => {
                fs::read_to_string(&full).map_err(|error| fail(format!("cannot be read as text: {error}")))?
            }
        };
        let patch = diffy::Patch::from_str(&section.text).map_err(|error| fail(error.to_string()))?;
        let patched = diffy::apply(&base, &patch)
            .map_err(|error| fail(format!("does not match the working tree ({error})")))?;
        if kind == ChangeKind::Delete && !patched.is_empty() {
            return Err(fail("the diff deletes the file but does not remove all of its lines".into()));
        }
        changes.push(Change {
            path: relative,
            kind,
            added: section.added,
            removed: section.removed,
            new_contents: (kind != ChangeKind::Delete).then_some(patched),
        });
    }
    if changes.is_empty() {
        return Err(PatchError::Empty);
    }
    Ok(Plan { changes })
}

impl Plan {
    /// Writes the plan. The caller is responsible for having checked the write permission.
    pub fn write(&self, workspace: &Workspace) -> Result<(), PatchError> {
        for change in &self.changes {
            let path = workspace.resolve(&change.path)?;
            match &change.new_contents {
                Some(contents) => {
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::write(path, contents)?;
                }
                None => fs::remove_file(path)?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::tests::{cleanup, sandbox};

    const MODIFY: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 1111111..2222222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -4,4 +4,5 @@ fn main() {

 pub fn add(a: i32, b: i32) -> i32 {
-    a + b
+    // Saturate instead of overflowing.
+    a.saturating_add(b)
 }
";

    const CREATE_AND_DELETE: &str = "\
--- /dev/null
+++ b/docs/NOTES.md
@@ -0,0 +1,2 @@
+# Notes
+--- not a header, just text
--- a/src/util.py
+++ /dev/null
@@ -1,3 +0,0 @@
-class Parser:
-    def parse(self):
-        pass
";

    #[test]
    fn a_plan_describes_the_change_and_writes_nothing() {
        let workspace = sandbox("patch-plan");
        let before = fs::read_to_string(workspace.root().join("src/main.rs")).unwrap();
        let plan = plan(&workspace, MODIFY).unwrap();
        assert_eq!(plan.changes.len(), 1);
        let change = &plan.changes[0];
        assert_eq!(
            (change.path.to_str(), change.kind, change.added, change.removed),
            (Some("src/main.rs"), ChangeKind::Modify, 2, 1)
        );
        assert_eq!(fs::read_to_string(workspace.root().join("src/main.rs")).unwrap(), before);

        plan.write(&workspace).unwrap();
        let after = fs::read_to_string(workspace.root().join("src/main.rs")).unwrap();
        assert!(after.contains("a.saturating_add(b)") && !after.contains("a + b"));
        assert!(after.starts_with("fn main() {\n"));
        cleanup(workspace);
    }

    #[test]
    fn files_can_be_created_and_deleted_in_one_diff() {
        let workspace = sandbox("patch-create");
        let plan = plan(&workspace, CREATE_AND_DELETE).unwrap();
        let kinds: Vec<_> = plan
            .changes
            .iter()
            .map(|change| (change.path.to_string_lossy().into_owned(), change.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("docs/NOTES.md".to_owned(), ChangeKind::Create),
                ("src/util.py".to_owned(), ChangeKind::Delete)
            ]
        );
        plan.write(&workspace).unwrap();
        assert_eq!(
            fs::read_to_string(workspace.root().join("docs/NOTES.md")).unwrap(),
            "# Notes\n--- not a header, just text\n"
        );
        assert!(!workspace.root().join("src/util.py").exists());
        cleanup(workspace);
    }

    #[test]
    fn a_diff_that_does_not_match_is_refused_as_a_whole() {
        let workspace = sandbox("patch-stale");
        let stale = format!("{}{}", CREATE_AND_DELETE, MODIFY.replace("    a + b", "    a - b"));
        let error = plan(&workspace, &stale).unwrap_err();
        assert!(matches!(&error, PatchError::DoesNotApply { path, .. } if path == "src/main.rs"), "{error}");
        // Nothing from the valid first part was written.
        assert!(!workspace.root().join("docs").exists());
        assert!(workspace.root().join("src/util.py").exists());
        cleanup(workspace);
    }

    #[test]
    fn paths_outside_the_workspace_and_broken_diffs_are_rejected() {
        let workspace = sandbox("patch-bad");
        let escape = "--- /dev/null\n+++ b/../../outside.txt\n@@ -0,0 +1 @@\n+x\n";
        assert!(matches!(plan(&workspace, escape), Err(PatchError::Workspace(WorkspaceError::Outside(_)))));
        let absolute = "--- /dev/null\n+++ /tmp/outside.txt\n@@ -0,0 +1 @@\n+x\n";
        assert!(matches!(plan(&workspace, absolute), Err(PatchError::Workspace(WorkspaceError::Outside(_)))));
        let exists = "--- /dev/null\n+++ b/Cargo.toml\n@@ -0,0 +1 @@\n+x\n";
        assert!(matches!(plan(&workspace, exists), Err(PatchError::DoesNotApply { .. })));
        let short = "--- a/Cargo.toml\n+++ b/Cargo.toml\n@@ -1,2 +1,3 @@\n [package]\n";
        assert!(matches!(plan(&workspace, short), Err(PatchError::Malformed { line: 5, .. })));
        assert!(matches!(plan(&workspace, "just some prose\n"), Err(PatchError::Empty)));
        cleanup(workspace);
    }

    #[test]
    fn hunk_headers() {
        assert_eq!(hunk_lengths("@@ -4,4 +4,5 @@ fn main() {"), Some((4, 5)));
        assert_eq!(hunk_lengths("@@ -1 +1 @@"), Some((1, 1)));
        assert_eq!(hunk_lengths("@@ -0,0 +1,2 @@"), Some((0, 2)));
        assert_eq!(hunk_lengths("@@ nonsense @@"), None);
    }
}
