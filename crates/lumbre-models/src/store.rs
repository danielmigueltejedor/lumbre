use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::{Manifest, Registry, Variant};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("download failed: {0}")]
    Http(String),
    #[error("the download was cancelled")]
    Cancelled,
    #[error("{file} is {found} bytes, the manifest says {expected}")]
    Size { file: String, expected: u64, found: u64 },
    #[error("{file} has SHA-256 {found}, the manifest says {expected}")]
    Digest { file: String, expected: String, found: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub done_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub id: String,
    pub quant: String,
    pub path: PathBuf,
    pub size_bytes: u64,
}

/// The on-disk model cache: `<root>/<model id>/<file>`.
#[derive(Clone, Debug)]
pub struct Store {
    root: PathBuf,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Feeds a file into a hasher; returns the number of bytes read.
fn hash_into(path: &Path, hasher: &mut Sha256) -> io::Result<u64> {
    let mut file = File::open(path)?;
    let mut buffer = vec![0u8; 1 << 20];
    let mut total = 0;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(total);
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self, manifest: &Manifest, variant: &Variant) -> PathBuf {
        self.root.join(&manifest.id).join(&variant.file)
    }

    /// Present with the size the manifest declares. Cheap; use [`Store::verify`] for the digest.
    pub fn is_installed(&self, manifest: &Manifest, variant: &Variant) -> bool {
        fs::metadata(self.path(manifest, variant))
            .is_ok_and(|meta| meta.is_file() && meta.len() == variant.size_bytes)
    }

    pub fn installed(&self, registry: &Registry) -> Vec<Installed> {
        let mut found = Vec::new();
        for manifest in registry.all() {
            for variant in manifest.variants.iter().filter(|variant| self.is_installed(manifest, variant)) {
                found.push(Installed {
                    id: manifest.id.clone(),
                    quant: variant.quant.clone(),
                    path: self.path(manifest, variant),
                    size_bytes: variant.size_bytes,
                });
            }
        }
        found
    }

    /// Bytes used by the whole cache, including partial downloads.
    pub fn disk_usage(&self) -> u64 {
        fn walk(dir: &Path) -> u64 {
            let Ok(entries) = fs::read_dir(dir) else { return 0 };
            entries
                .flatten()
                .map(|entry| match entry.metadata() {
                    Ok(meta) if meta.is_dir() => walk(&entry.path()),
                    Ok(meta) => meta.len(),
                    Err(_) => 0,
                })
                .sum()
        }
        walk(&self.root)
    }

    /// Re-reads an installed file and checks size and SHA-256 against the manifest.
    pub fn verify(&self, manifest: &Manifest, variant: &Variant) -> Result<(), StoreError> {
        let mut hasher = Sha256::new();
        let size = hash_into(&self.path(manifest, variant), &mut hasher)?;
        check(variant, size, &hex(&hasher.finalize()))
    }

    /// Deletes a variant and any partial download of it. Returns whether anything was removed.
    pub fn remove(&self, manifest: &Manifest, variant: &Variant) -> Result<bool, StoreError> {
        let path = self.path(manifest, variant);
        let mut removed = false;
        for target in [path.clone(), partial(&path)] {
            match fs::remove_file(&target) {
                Ok(()) => removed = true,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        if let Some(dir) = path.parent() {
            // Only succeeds when the directory is empty, which is exactly when it should go.
            let _ = fs::remove_dir(dir);
        }
        Ok(removed)
    }

    /// Downloads a variant from the manifest's pinned URL. See [`Store::download_from`].
    pub fn download(
        &self,
        manifest: &Manifest,
        variant: &Variant,
        on_progress: &mut dyn FnMut(Progress) -> bool,
    ) -> Result<PathBuf, StoreError> {
        self.download_from(&manifest.url(variant), manifest, variant, on_progress)
    }

    /// Downloads to `<file>.part`, resuming a previous attempt, hashing as it goes, and only
    /// renames the file into place once size and SHA-256 match the manifest. A mismatch deletes
    /// the partial file. `on_progress` returns `false` to cancel (the partial file is kept).
    pub fn download_from(
        &self,
        url: &str,
        manifest: &Manifest,
        variant: &Variant,
        on_progress: &mut dyn FnMut(Progress) -> bool,
    ) -> Result<PathBuf, StoreError> {
        let path = self.path(manifest, variant);
        if self.is_installed(manifest, variant) {
            return Ok(path);
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let part = partial(&path);
        let mut hasher = Sha256::new();
        let mut done = match fs::metadata(&part) {
            // A partial file at least as large as the target cannot be a prefix of it.
            Ok(meta) if meta.len() < variant.size_bytes => hash_into(&part, &mut hasher)?,
            Ok(_) => {
                fs::remove_file(&part)?;
                0
            }
            Err(_) => 0,
        };

        let mut request = ureq::get(url);
        if done > 0 {
            request = request.header("Range", format!("bytes={done}-"));
        }
        let mut response = request.call().map_err(|error| StoreError::Http(error.to_string()))?;
        if done > 0 && response.status().as_u16() != 206 {
            // The server ignored the range: start again from the first byte.
            hasher = Sha256::new();
            done = 0;
        }
        let mut file =
            if done > 0 { OpenOptions::new().append(true).open(&part)? } else { File::create(&part)? };

        let total = variant.size_bytes;
        let mut reader = response.body_mut().as_reader();
        let mut buffer = vec![0u8; 1 << 16];
        loop {
            let read = reader.read(&mut buffer).map_err(|error| StoreError::Http(error.to_string()))?;
            if read == 0 {
                break;
            }
            done += read as u64;
            if done > total {
                // Never write more than the manifest promises, whatever the server sends.
                drop(file);
                fs::remove_file(&part)?;
                return Err(StoreError::Size { file: variant.file.clone(), expected: total, found: done });
            }
            file.write_all(&buffer[..read])?;
            hasher.update(&buffer[..read]);
            if !on_progress(Progress { done_bytes: done, total_bytes: total }) {
                file.sync_all()?;
                return Err(StoreError::Cancelled);
            }
        }
        file.sync_all()?;
        drop(file);

        if done < total {
            // The connection ended early; keep the partial file so the next attempt resumes.
            return Err(StoreError::Http(format!("connection closed after {done} of {total} bytes")));
        }
        if let Err(error) = check(variant, done, &hex(&hasher.finalize())) {
            fs::remove_file(&part)?;
            return Err(error);
        }
        fs::rename(&part, &path)?;
        Ok(path)
    }
}

fn partial(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

fn check(variant: &Variant, size: u64, digest: &str) -> Result<(), StoreError> {
    if size != variant.size_bytes {
        return Err(StoreError::Size {
            file: variant.file.clone(),
            expected: variant.size_bytes,
            found: size,
        });
    }
    if digest != variant.sha256 {
        return Err(StoreError::Digest {
            file: variant.file.clone(),
            expected: variant.sha256.clone(),
            found: digest.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumbre_router::Tier;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// A manifest for an arbitrary payload, so tests exercise the real code path on small files.
    fn manifest_for(payload: &[u8]) -> Manifest {
        Manifest {
            schema: 1,
            id: "test-model".into(),
            name: "Test".into(),
            tier: Tier::Nano,
            publisher: "tests".into(),
            upstream: "https://example.invalid/up".into(),
            source: "https://example.invalid/src".into(),
            revision: "abc123".into(),
            license: "Apache-2.0".into(),
            license_url: "https://example.invalid/LICENSE".into(),
            format: "gguf".into(),
            parameters: "0".into(),
            variants: vec![Variant {
                quant: "Q4".into(),
                file: "test.gguf".into(),
                size_bytes: payload.len() as u64,
                sha256: hex(&Sha256::digest(payload)),
            }],
        }
    }

    fn temp_store(name: &str) -> Store {
        let root = std::env::temp_dir().join(format!("lumbre-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        Store::new(root)
    }

    /// A minimal HTTP/1.1 file server on loopback that honours `Range: bytes=N-` and records the
    /// range of every request. `cut_at` makes the first response stop early, like a dropped link.
    fn serve(payload: Vec<u8>, cut_at: Option<usize>) -> (String, Arc<Mutex<Vec<Option<u64>>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/test.gguf", listener.local_addr().unwrap());
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&ranges);
        std::thread::spawn(move || {
            let mut cut_at = cut_at;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut range = None;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        range = value.trim().trim_end_matches('-').parse::<u64>().ok();
                    }
                }
                seen.lock().unwrap().push(range);
                let start = range.unwrap_or(0) as usize;
                let body = &payload[start..];
                let status = if range.is_some() { "206 Partial Content" } else { "200 OK" };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body[..cut_at.take().unwrap_or(body.len()).min(body.len())]);
            }
        });
        (url, ranges)
    }

    fn payload() -> Vec<u8> {
        (0..300_000u32).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn download_verifies_installs_lists_and_removes() {
        let data = payload();
        let manifest = manifest_for(&data);
        let variant = &manifest.variants[0];
        let store = temp_store("ok");
        let (url, _) = serve(data.clone(), None);

        let mut last = None;
        let path = store.download_from(&url, &manifest, variant, &mut |progress| {
            last = Some(progress);
            true
        });
        let path = path.unwrap();
        assert_eq!(fs::read(&path).unwrap(), data);
        assert_eq!(last, Some(Progress { done_bytes: 300_000, total_bytes: 300_000 }));
        assert!(store.is_installed(&manifest, variant));
        store.verify(&manifest, variant).unwrap();
        assert_eq!(store.disk_usage(), 300_000);

        let mut registry = Registry::default();
        registry.add(manifest.clone());
        assert_eq!(store.installed(&registry).len(), 1);

        assert!(store.remove(&manifest, variant).unwrap());
        assert!(!store.remove(&manifest, variant).unwrap());
        assert!(store.installed(&registry).is_empty());
        assert!(!store.root().join("test-model").exists());
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn an_interrupted_download_resumes_where_it_stopped() {
        let data = payload();
        let manifest = manifest_for(&data);
        let variant = &manifest.variants[0];
        let store = temp_store("resume");
        let (url, ranges) = serve(data.clone(), Some(100_000));

        let first = store.download_from(&url, &manifest, variant, &mut |_| true).unwrap_err();
        assert!(matches!(first, StoreError::Http(_)), "{first}");
        assert!(!store.is_installed(&manifest, variant));

        let path = store.download_from(&url, &manifest, variant, &mut |_| true).unwrap();
        assert_eq!(fs::read(path).unwrap(), data);
        assert_eq!(*ranges.lock().unwrap(), [None, Some(100_000)]);
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn a_corrupted_download_is_rejected_and_leaves_nothing_behind() {
        let data = payload();
        let manifest = manifest_for(&data);
        let variant = &manifest.variants[0];
        let store = temp_store("corrupt");
        let mut tampered = data.clone();
        tampered[1234] ^= 0xff;
        let (url, _) = serve(tampered, None);

        let error = store.download_from(&url, &manifest, variant, &mut |_| true).unwrap_err();
        assert!(matches!(error, StoreError::Digest { .. }), "{error}");
        assert!(!store.path(&manifest, variant).exists());
        assert!(!partial(&store.path(&manifest, variant)).exists());
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn an_oversized_response_is_cut_off() {
        let data = payload();
        let manifest = manifest_for(&data[..1000]);
        let store = temp_store("oversize");
        let (url, _) = serve(data, None);
        let error = store.download_from(&url, &manifest, &manifest.variants[0], &mut |_| true).unwrap_err();
        assert!(matches!(error, StoreError::Size { .. }), "{error}");
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn cancelling_keeps_the_partial_file_for_later() {
        let data = payload();
        let manifest = manifest_for(&data);
        let variant = &manifest.variants[0];
        let store = temp_store("cancel");
        let (url, _) = serve(data, None);
        let error = store.download_from(&url, &manifest, variant, &mut |_| false).unwrap_err();
        assert!(matches!(error, StoreError::Cancelled));
        assert!(partial(&store.path(&manifest, variant)).exists());
        assert!(store.verify(&manifest, variant).is_err());
        let _ = fs::remove_dir_all(store.root());
    }
}
