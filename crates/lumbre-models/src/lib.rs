//! Lumbre Models: which open-weight models exist, which are on disk, and how they get there.
//!
//! * [`Manifest`] — a versioned description of one upstream model: who published it, under which
//!   licence, at which pinned revision, and the size and SHA-256 of every quantisation.
//! * [`Registry`] — the manifests Lumbre knows (built in, plus any the user adds).
//! * [`Store`] — the local cache: download on demand with resume, verify, list and delete.
//!
//! Weights never live in Git. A manifest is the only thing the repository carries.

mod manifest;
mod store;

use lumbre_router::Tier;

pub use manifest::{Manifest, ManifestError, Variant, SCHEMA};
pub use store::{Installed, Progress, Store, StoreError};

const MIB: u64 = 1 << 20;

/// Rough resident memory of a variant once loaded with `context_tokens` of context.
///
/// A heuristic, not a measurement: file size plus 5 %, a context cache that grows with the model,
/// and a fixed 200 MiB for the runtime. It errs on the high side so the governor does not
/// overcommit. Measured per-hardware profiles replace it when they exist (planned for 0.2).
pub fn estimate_ram_bytes(variant: &Variant, context_tokens: u32) -> u64 {
    let size = variant.size_bytes;
    size + size / 20 + u64::from(context_tokens) * (size / 20_000) + 200 * MIB
}

#[derive(Clone, Debug, Default)]
pub struct Registry {
    manifests: Vec<Manifest>,
}

/// The outcome of [`Registry::pick`].
#[derive(Clone, Debug, PartialEq)]
pub struct Pick<'a> {
    pub manifest: &'a Manifest,
    pub variant: &'a Variant,
    /// True when the requested tier was not available and a smaller one was chosen.
    pub downgraded: bool,
}

impl Registry {
    /// The manifests shipped in `models/`.
    pub fn builtin() -> Self {
        const SOURCES: [&str; 6] = [
            include_str!("../../../models/qwen3-0.6b.toml"),
            include_str!("../../../models/qwen3-1.7b.toml"),
            include_str!("../../../models/qwen3-4b.toml"),
            include_str!("../../../models/qwen3-8b.toml"),
            include_str!("../../../models/qwen2.5-coder-1.5b.toml"),
            include_str!("../../../models/qwen2.5-coder-7b.toml"),
        ];
        let manifests = SOURCES
            .iter()
            .map(|text| {
                Manifest::parse(text).unwrap_or_else(|error| panic!("built-in manifest is invalid: {error}"))
            })
            .collect();
        Self { manifests }
    }

    /// Adds every `*.toml` manifest in a directory; a later id replaces an earlier one.
    /// Returns the files that could not be read, so the caller can report them.
    pub fn load_dir(&mut self, dir: &std::path::Path) -> Vec<(std::path::PathBuf, ManifestError)> {
        let mut failures = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else { return failures };
        let mut paths: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
        paths.sort();
        for path in paths.into_iter().filter(|path| path.extension().is_some_and(|ext| ext == "toml")) {
            let parsed = std::fs::read_to_string(&path)
                .map_err(|error| ManifestError::Invalid(error.to_string()))
                .and_then(|text| Manifest::parse(&text));
            match parsed {
                Ok(manifest) => self.add(manifest),
                Err(error) => failures.push((path, error)),
            }
        }
        failures
    }

    pub fn add(&mut self, manifest: Manifest) {
        self.manifests.retain(|existing| existing.id != manifest.id);
        self.manifests.push(manifest);
    }

    pub fn all(&self) -> &[Manifest] {
        &self.manifests
    }

    pub fn get(&self, id: &str) -> Option<&Manifest> {
        self.manifests.iter().find(|manifest| manifest.id == id)
    }

    /// The best variant for a tier that satisfies `usable` (typically "is installed") and whose
    /// estimated memory fits `budget_bytes`: the requested tier first, then smaller ones; within
    /// a tier, the largest model and the highest-quality quantisation that fit.
    pub fn pick(
        &self,
        tier: Tier,
        budget_bytes: u64,
        context_tokens: u32,
        usable: impl Fn(&Manifest, &Variant) -> bool,
    ) -> Option<Pick<'_>> {
        for candidate_tier in tier.fallbacks() {
            let mut options: Vec<(&Manifest, &Variant)> = self
                .manifests
                .iter()
                .filter(|manifest| manifest.tier == *candidate_tier)
                .flat_map(|manifest| manifest.variants.iter().map(move |variant| (manifest, variant)))
                .filter(|(manifest, variant)| {
                    estimate_ram_bytes(variant, context_tokens) <= budget_bytes && usable(manifest, variant)
                })
                .collect();
            options.sort_by_key(|(_, variant)| std::cmp::Reverse(variant.size_bytes));
            if let Some((manifest, variant)) = options.first() {
                return Some(Pick { manifest, variant, downgraded: *candidate_tier != tier });
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    #[test]
    fn builtin_manifests_are_valid_and_cover_every_tier() {
        let registry = Registry::builtin();
        assert_eq!(registry.all().len(), 6);
        for tier in Tier::ALL {
            assert!(registry.all().iter().any(|manifest| manifest.tier == tier), "{tier:?}");
        }
        for manifest in registry.all() {
            // Everything Lumbre points at by default must be redistributable.
            assert_eq!(manifest.license, "Apache-2.0", "{}", manifest.id);
            assert_eq!(manifest.revision.len(), 40, "{}", manifest.id);
        }
    }

    #[test]
    fn the_estimate_grows_with_size_and_context() {
        let registry = Registry::builtin();
        let q4 = registry.get("qwen3-4b").unwrap().variant("Q4_K_M").unwrap();
        let small = estimate_ram_bytes(q4, 2048);
        let large = estimate_ram_bytes(q4, 8192);
        assert!(small > q4.size_bytes && large > small);
        // 4B Q4_K_M with a 4k context: between 3 and 3.5 GiB.
        let gib = estimate_ram_bytes(q4, 4096) as f64 / GIB as f64;
        assert!((3.0..3.5).contains(&gib), "{gib}");
    }

    #[test]
    fn pick_prefers_quality_within_budget_and_falls_back_to_smaller_tiers() {
        let registry = Registry::builtin();
        let any = |_: &Manifest, _: &Variant| true;

        let roomy = registry.pick(Tier::Standard, 16 * GIB, 4096, any).unwrap();
        assert_eq!(
            (roomy.manifest.id.as_str(), roomy.variant.quant.as_str(), roomy.downgraded),
            ("qwen3-4b", "Q8_0", false)
        );

        let tight = registry.pick(Tier::Standard, 7 * GIB / 2, 4096, any).unwrap();
        assert_eq!((tight.manifest.id.as_str(), tight.variant.quant.as_str()), ("qwen3-4b", "Q4_K_M"));

        let tiny = registry.pick(Tier::Large, 2 * GIB, 4096, any).unwrap();
        assert_eq!((tiny.manifest.id.as_str(), tiny.downgraded), ("qwen3-0.6b", true));

        assert!(registry.pick(Tier::Large, 500 * MIB, 4096, any).is_none());
    }

    #[test]
    fn pick_only_returns_usable_variants() {
        let registry = Registry::builtin();
        let only_small_coder = |manifest: &Manifest, _: &Variant| manifest.id == "qwen2.5-coder-1.5b";
        let pick = registry.pick(Tier::Code, 16 * GIB, 4096, only_small_coder).unwrap();
        assert_eq!(
            (pick.manifest.id.as_str(), pick.variant.quant.as_str(), pick.downgraded),
            ("qwen2.5-coder-1.5b", "Q8_0", false)
        );
        assert!(registry.pick(Tier::Code, 16 * GIB, 4096, |_, _| false).is_none());
    }

    #[test]
    fn user_manifests_override_builtin_ones_and_bad_files_are_reported() {
        let dir = std::env::temp_dir().join(format!("lumbre-models-registry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let custom = include_str!("../../../models/qwen3-0.6b.toml").replace("Qwen3 0.6B", "My build");
        std::fs::write(dir.join("a.toml"), custom).unwrap();
        std::fs::write(dir.join("b.toml"), "schema = 99").unwrap();
        let mut registry = Registry::builtin();
        let failures = registry.load_dir(&dir);
        assert_eq!(failures.len(), 1);
        assert_eq!(registry.all().len(), 6);
        assert_eq!(registry.get("qwen3-0.6b").unwrap().name, "My build");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
