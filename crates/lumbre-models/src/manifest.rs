use lumbre_router::Tier;
use serde::{Deserialize, Serialize};

/// The manifest format version this build understands.
pub const SCHEMA: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("not a valid manifest: {0}")]
    Invalid(String),
    #[error("manifest schema {found} is not supported (this build understands {SCHEMA})")]
    Schema { found: u32 },
}

/// One upstream model and the files Lumbre can fetch for it.
///
/// Provenance is part of the format on purpose: Lumbre's profile names (Nano, Mini, …) are
/// labels for a size class, and the manifest always says whose model is actually behind them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    /// Stable identifier: lowercase letters, digits, `.` and `-`.
    pub id: String,
    pub name: String,
    pub tier: Tier,
    pub publisher: String,
    /// The original model card.
    pub upstream: String,
    /// The repository the files are downloaded from.
    pub source: String,
    /// Commit of `source` the digests below belong to.
    pub revision: String,
    /// SPDX identifier of the weights' licence.
    pub license: String,
    pub license_url: String,
    pub format: String,
    pub parameters: String,
    #[serde(rename = "variant")]
    pub variants: Vec<Variant>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Variant {
    /// Quantisation label, e.g. `Q4_K_M`.
    pub quant: String,
    pub file: String,
    pub size_bytes: u64,
    /// Lowercase hex SHA-256 of the file.
    pub sha256: String,
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        // Check the schema before the full shape, so a newer manifest gets a useful error.
        #[derive(Deserialize)]
        struct Header {
            schema: u32,
        }
        let header: Header =
            toml::from_str(text).map_err(|error| ManifestError::Invalid(error.to_string()))?;
        if header.schema != SCHEMA {
            return Err(ManifestError::Schema { found: header.schema });
        }
        let manifest: Manifest =
            toml::from_str(text).map_err(|error| ManifestError::Invalid(error.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<(), ManifestError> {
        let fail = |message: String| Err(ManifestError::Invalid(message));
        let id_ok = |ch: char| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '.' || ch == '-';
        if self.id.is_empty() || !self.id.chars().all(id_ok) || self.id.starts_with('.') {
            return fail(format!("id `{}` must be lowercase letters, digits, `.` or `-`", self.id));
        }
        if !self.source.starts_with("https://") {
            return fail(format!("source `{}` must be an https URL", self.source));
        }
        if self.revision.is_empty() || !self.revision.chars().all(|ch| ch.is_ascii_alphanumeric()) {
            return fail("revision must be a commit hash".into());
        }
        if self.license.trim().is_empty() {
            return fail("a manifest must state the licence of the weights".into());
        }
        if self.variants.is_empty() {
            return fail("a manifest needs at least one variant".into());
        }
        for variant in &self.variants {
            // The file name becomes part of a local path and of a URL.
            let name_ok = |ch: char| ch.is_ascii_alphanumeric() || "._-".contains(ch);
            if variant.file.is_empty() || variant.file.starts_with('.') || !variant.file.chars().all(name_ok)
            {
                return fail(format!("file name `{}` is not a plain file name", variant.file));
            }
            let hex_ok = |ch: char| ch.is_ascii_digit() || ('a'..='f').contains(&ch);
            if variant.sha256.len() != 64 || !variant.sha256.chars().all(hex_ok) {
                return fail(format!("sha256 of `{}` must be 64 lowercase hex digits", variant.file));
            }
            if variant.size_bytes == 0 {
                return fail(format!("size of `{}` must not be zero", variant.file));
            }
            if self.variants.iter().filter(|other| other.quant == variant.quant).count() > 1 {
                return fail(format!("quantisation `{}` appears twice", variant.quant));
            }
        }
        Ok(())
    }

    pub fn variant(&self, quant: &str) -> Option<&Variant> {
        self.variants.iter().find(|variant| variant.quant.eq_ignore_ascii_case(quant))
    }

    /// Where a variant is downloaded from: the pinned revision, never a moving branch.
    pub fn url(&self, variant: &Variant) -> String {
        format!("{}/resolve/{}/{}", self.source.trim_end_matches('/'), self.revision, variant.file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = include_str!("../../../models/qwen3-4b.toml");

    #[test]
    fn a_builtin_manifest_round_trips() {
        let manifest = Manifest::parse(GOOD).unwrap();
        assert_eq!(manifest.id, "qwen3-4b");
        assert_eq!(manifest.tier, Tier::Standard);
        assert_eq!(manifest.variants.len(), 4);
        let q4 = manifest.variant("q4_k_m").unwrap();
        assert_eq!(
            manifest.url(q4),
            "https://huggingface.co/Qwen/Qwen3-4B-GGUF/resolve/bc640142c66e1fdd12af0bd68f40445458f3869b/Qwen3-4B-Q4_K_M.gguf"
        );
    }

    #[test]
    fn broken_manifests_are_rejected_with_a_reason() {
        let cases = [
            ("schema = 1", "schema = 2", "schema 2 is not supported"),
            ("id = \"qwen3-4b\"", "id = \"../etc\"", "must be lowercase"),
            ("https://huggingface.co/Qwen/Qwen3-4B-GGUF\"", "http://example.org/x\"", "https URL"),
            ("file = \"Qwen3-4B-Q4_K_M.gguf\"", "file = \"../../x.gguf\"", "plain file name"),
            ("7485fe6f", "XYZ5fe6f", "64 lowercase hex"),
            ("size_bytes = 2497280256", "size_bytes = 0", "must not be zero"),
            ("quant = \"Q5_K_M\"", "quant = \"Q4_K_M\"", "appears twice"),
            ("license = \"Apache-2.0\"", "license = \"\"", "licence"),
            ("format = \"gguf\"", "format = \"gguf\"\nsurprise = 1", "unknown field"),
        ];
        for (from, to, expected) in cases {
            assert!(GOOD.contains(from), "fixture lacks `{from}`");
            let error = Manifest::parse(&GOOD.replacen(from, to, 1)).unwrap_err().to_string();
            assert!(error.contains(expected), "`{to}` gave: {error}");
        }
    }
}
