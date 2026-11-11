use std::env;
use std::path::PathBuf;

/// Where Lumbre keeps its files.
///
/// | | Linux | macOS | Windows |
/// | --- | --- | --- | --- |
/// | config | `$XDG_CONFIG_HOME/lumbre` | `~/Library/Application Support/Lumbre` | `%APPDATA%\Lumbre` |
/// | data | `$XDG_DATA_HOME/lumbre` | same | `%LOCALAPPDATA%\Lumbre` |
///
/// `$LUMBRE_HOME`, when set, replaces both with one directory (used by tests and portable
/// installs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

fn home() -> PathBuf {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

fn var_dir(name: &str) -> Option<PathBuf> {
    env::var_os(name).map(PathBuf::from).filter(|path| path.is_absolute())
}

impl Paths {
    pub fn at(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self { config_dir: root.clone(), data_dir: root }
    }

    pub fn discover() -> Self {
        if let Some(root) = var_dir("LUMBRE_HOME") {
            return Self::at(root);
        }
        if cfg!(target_os = "macos") {
            Self::at(home().join("Library/Application Support/Lumbre"))
        } else if cfg!(windows) {
            Self {
                config_dir: var_dir("APPDATA").unwrap_or_else(home).join("Lumbre"),
                data_dir: var_dir("LOCALAPPDATA").unwrap_or_else(home).join("Lumbre"),
            }
        } else {
            Self {
                config_dir: var_dir("XDG_CONFIG_HOME")
                    .unwrap_or_else(|| home().join(".config"))
                    .join("lumbre"),
                data_dir: var_dir("XDG_DATA_HOME")
                    .unwrap_or_else(|| home().join(".local/share"))
                    .join("lumbre"),
            }
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// User-provided model manifests, in addition to the built-in ones.
    pub fn manifests_dir(&self) -> PathBuf {
        self.config_dir.join("models")
    }

    /// Downloaded weights.
    pub fn models_dir(&self) -> PathBuf {
        self.data_dir.join("models")
    }

    pub fn conversations_dir(&self) -> PathBuf {
        self.data_dir.join("conversations")
    }

    /// The bearer token for the local API.
    pub fn token_file(&self) -> PathBuf {
        self.data_dir.join("api-token")
    }

    /// Written by the daemon with the address it listens on.
    pub fn endpoint_file(&self) -> PathBuf {
        self.data_dir.join("endpoint")
    }
}
