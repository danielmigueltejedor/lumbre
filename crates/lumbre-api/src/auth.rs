use std::fs::{self, OpenOptions};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write};
use std::path::Path;

/// 128 bits from the operating system's generator, as hex.
///
/// `RandomState` is seeded from the OS; hashing nothing with fresh states exposes that seed
/// without adding a dependency or `unsafe`.
fn random_token() -> String {
    (0..4)
        .map(|_| format!("{:016x}", std::collections::hash_map::RandomState::new().build_hasher().finish()))
        .collect()
}

/// Reads the API token, creating it (readable by the owner only) on first use.
///
/// Loopback is reachable by every local account, so the token is what restricts the API to the
/// user who owns the files. Whoever comes first — daemon or client — creates it.
pub fn load_or_create_token(path: &Path) -> io::Result<String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            let token = random_token();
            file.write_all(token.as_bytes())?;
            Ok(token)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let token = fs::read_to_string(path)?.trim().to_owned();
            if token.len() < 32 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the API token file is damaged; delete it",
                ));
            }
            Ok(token)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_is_created_once_private_and_then_reused() {
        let dir = std::env::temp_dir().join(format!("lumbre-auth-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("nested/api-token");
        let first = load_or_create_token(&path).unwrap();
        assert_eq!(first.len(), 64);
        assert!(first.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert_eq!(load_or_create_token(&path).unwrap(), first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_ne!(random_token(), random_token());
        fs::write(&path, "short").unwrap();
        assert!(load_or_create_token(&path).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
