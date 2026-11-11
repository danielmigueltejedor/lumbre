use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum PermissionError {
    #[error("could not read {path}: {reason}")]
    Read { path: PathBuf, reason: String },
    #[error("could not write {path}: {reason}")]
    Write { path: PathBuf, reason: String },
}

/// What the user allowed in one repository. The default allows nothing but reading.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Grant {
    /// Apply diffs: create, modify and delete files inside the repository.
    pub write: bool,
    /// Command prefixes that may be executed, e.g. `cargo test`. A command is allowed when its
    /// words start with all the words of one prefix.
    pub exec: Vec<String>,
}

impl Grant {
    pub fn allows_exec(&self, argv: &[String]) -> bool {
        self.exec.iter().any(|prefix| {
            let words: Vec<&str> = prefix.split_whitespace().collect();
            !words.is_empty()
                && argv.len() >= words.len()
                && words.iter().zip(argv).all(|(word, arg)| word == arg)
        })
    }
}

/// Grants for every repository, stored in the **user's** configuration directory and keyed by
/// the repository's absolute path. A repository cannot carry its own grants: a project you just
/// cloned has none, whatever files it contains.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Permissions {
    repo: BTreeMap<String, Grant>,
    #[serde(skip)]
    file: PathBuf,
}

fn key(root: &Path) -> String {
    root.to_string_lossy().into_owned()
}

impl Permissions {
    /// Loads the grants file; a missing file means no grants.
    pub fn load(file: impl Into<PathBuf>) -> Result<Self, PermissionError> {
        let file = file.into();
        let text = match fs::read_to_string(&file) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(PermissionError::Read { path: file, reason: error.to_string() }),
        };
        let mut permissions: Permissions = toml::from_str(&text)
            .map_err(|error| PermissionError::Read { path: file.clone(), reason: error.to_string() })?;
        permissions.file = file;
        Ok(permissions)
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    /// The grant for a repository (by canonical root); nothing granted if it has none.
    pub fn grant(&self, root: &Path) -> Grant {
        self.repo.get(&key(root)).cloned().unwrap_or_default()
    }

    /// Changes a repository's grant and saves the file.
    pub fn update(&mut self, root: &Path, change: impl FnOnce(&mut Grant)) -> Result<Grant, PermissionError> {
        let grant = self.repo.entry(key(root)).or_default();
        change(grant);
        grant.exec.sort();
        grant.exec.dedup();
        let result = grant.clone();
        if result == Grant::default() {
            self.repo.remove(&key(root));
        }
        self.save()?;
        Ok(result)
    }

    fn save(&self) -> Result<(), PermissionError> {
        let fail = |reason: String| PermissionError::Write { path: self.file.clone(), reason };
        if let Some(dir) = self.file.parent() {
            fs::create_dir_all(dir).map_err(|error| fail(error.to_string()))?;
        }
        let text = toml::to_string_pretty(self).map_err(|error| fail(error.to_string()))?;
        fs::write(&self.file, text).map_err(|error| fail(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(command: &str) -> Vec<String> {
        command.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn exec_prefixes_match_whole_words_only() {
        let grant = Grant { write: false, exec: vec!["cargo test".into(), "make".into()] };
        assert!(grant.allows_exec(&argv("cargo test")));
        assert!(grant.allows_exec(&argv("cargo test -p lumbre-core")));
        assert!(grant.allows_exec(&argv("make check")));
        assert!(!grant.allows_exec(&argv("cargo testify")));
        assert!(!grant.allows_exec(&argv("cargo")));
        assert!(!grant.allows_exec(&argv("cargo run")));
        assert!(!grant.allows_exec(&argv("sh -c cargo test")));
        assert!(!Grant::default().allows_exec(&argv("ls")));
        assert!(!Grant { write: true, exec: vec![String::new()] }.allows_exec(&argv("ls")));
    }

    #[test]
    fn grants_are_per_repository_persisted_and_revocable() {
        let dir = std::env::temp_dir().join(format!("lumbre-code-perms-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let file = dir.join("code-permissions.toml");
        let (mine, other) = (Path::new("/home/me/project"), Path::new("/home/me/cloned"));

        let mut permissions = Permissions::load(&file).unwrap();
        assert_eq!(permissions.grant(mine), Grant::default());
        permissions
            .update(mine, |grant| {
                grant.write = true;
                grant.exec.push("cargo test".into());
                grant.exec.push("cargo test".into());
            })
            .unwrap();

        let reloaded = Permissions::load(&file).unwrap();
        assert_eq!(reloaded.grant(mine), Grant { write: true, exec: vec!["cargo test".into()] });
        assert_eq!(reloaded.grant(other), Grant::default());

        let mut reloaded = reloaded;
        reloaded.update(mine, |grant| *grant = Grant::default()).unwrap();
        assert!(!fs::read_to_string(&file).unwrap().contains("/home/me/project"));

        fs::write(&file, "repo = 3").unwrap();
        assert!(matches!(Permissions::load(&file), Err(PermissionError::Read { .. })));
        fs::remove_dir_all(dir).unwrap();
    }
}
