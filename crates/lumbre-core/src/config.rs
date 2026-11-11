use std::path::{Path, PathBuf};
use std::time::Duration;

use lumbre_resources::{Mode, Policy};
use serde::{Deserialize, Serialize};

const MIB: u64 = 1 << 20;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read {path}: {error}")]
    Read { path: PathBuf, error: std::io::Error },
    #[error("{path} is not valid: {error}")]
    Parse { path: PathBuf, error: String },
}

/// `config.toml`. Every key is optional; a missing file is the default configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// `auto`, `performance` or `saver`.
    pub mode: Mode,
    /// Hard ceiling for model memory, in MiB.
    pub max_ram_mb: Option<u64>,
    /// Memory always left to other applications, in MiB. Default: 15 % of RAM, at least 2048.
    pub reserve_mb: Option<u64>,
    /// Unload a model nobody used for this long.
    pub idle_unload_secs: u64,
    /// Context window to load models with. Larger contexts cost memory.
    pub context_tokens: u32,
    /// Upper bound on the length of an answer.
    pub max_tokens: u32,
    /// Model layers to offload to a GPU. 0 runs on the CPU only.
    pub gpu_layers: u32,
    /// Path to llama.cpp's `llama-server`, when it is not on `PATH`.
    pub llama_server: Option<PathBuf>,
    /// Folders `files.find` may look into. Empty means file search is off.
    pub search_roots: Vec<PathBuf>,
    /// Loopback port of the local API.
    pub port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: Mode::Auto,
            max_ram_mb: None,
            reserve_mb: None,
            idle_unload_secs: 300,
            context_tokens: 4096,
            max_tokens: 1024,
            gpu_layers: 0,
            llama_server: None,
            search_roots: Vec::new(),
            port: lumbre_api::DEFAULT_PORT,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(ConfigError::Read { path: path.to_owned(), error }),
        };
        toml::from_str(&text)
            .map_err(|error| ConfigError::Parse { path: path.to_owned(), error: error.to_string() })
    }

    pub fn policy(&self) -> Policy {
        Policy {
            mode: self.mode,
            max_ram_bytes: self.max_ram_mb.map(|mb| mb * MIB),
            reserve_bytes: self.reserve_mb.map(|mb| mb * MIB),
            idle_unload: Duration::from_secs(self.idle_unload_secs),
            ..Policy::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_the_default_and_typos_are_errors() {
        let dir = std::env::temp_dir().join(format!("lumbre-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let _ = std::fs::remove_file(&path);
        assert_eq!(Config::load(&path).unwrap(), Config::default());

        std::fs::write(
            &path,
            "mode = \"saver\"\nmax_ram_mb = 4096\nsearch_roots = [\"/home/me/Documents\"]\n",
        )
        .unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.mode, Mode::Saver);
        assert_eq!(config.policy().max_ram_bytes, Some(4096 * MIB));
        assert_eq!(config.search_roots, [PathBuf::from("/home/me/Documents")]);

        std::fs::write(&path, "max_ram = 4096\n").unwrap();
        let error = Config::load(&path).unwrap_err().to_string();
        assert!(error.contains("unknown field `max_ram`"), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
