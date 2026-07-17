//! The mock embedding: a fixed deterministic wire function (ADR 0014/0019).
//!
//! Embeddings **bypass the behavior seam entirely** and are never
//! scenario-overridable — they are computation, not behavior. The vector is a
//! **seed-independent** lexical-overlap projection (signed feature-hashing):
//! lowercase, split on non-alphanumeric, FNV-1a-64 per token into `D` signed
//! buckets, L2-normalize. Identical text always yields the identical vector,
//! and similarity is honest lexical overlap — the most a content-blind mock
//! can plausibly promise.

use crate::seed::fnv1a64;

/// The default vector dimensionality (ADR 0014); the request's `dimensions`
/// overrides it.
pub const DEFAULT_DIMENSIONS: usize = 256;

/// Embed one text: signed feature-hashing into `dimensions` buckets, then
/// L2-normalization. Pure and seed-independent by construction — it takes no
/// identity input at all.
pub fn embed_text(text: &str, dimensions: usize) -> Vec<f32> {
    if dimensions == 0 {
        return Vec::new();
    }
    let mut vector = vec![0.0_f32; dimensions];
    let lowered = text.to_lowercase();
    for token in lowered
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
    {
        let hash = fnv1a64(token.as_bytes());
        let bucket = (hash % dimensions as u64) as usize;
        // The sign comes from a bit uncorrelated with the bucket index.
        let sign = if hash >> 63 == 1 { -1.0 } else { 1.0 };
        vector[bucket] += sign;
    }
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value /= norm;
        }
    }
    vector
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    #[test]
    fn identical_text_yields_the_identical_vector() {
        let text = "Setup: install mise; then `mise run build` / `test`.";
        assert_eq!(
            embed_text(text, DEFAULT_DIMENSIONS),
            embed_text(text, DEFAULT_DIMENSIONS)
        );
    }

    #[test]
    fn vectors_are_l2_normalized() {
        let vector = embed_text("install mise and build the workspace", DEFAULT_DIMENSIONS);
        let norm: f32 = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm was {norm}");
    }

    #[test]
    fn empty_text_is_the_zero_vector() {
        let vector = embed_text("", DEFAULT_DIMENSIONS);
        assert_eq!(vector.len(), DEFAULT_DIMENSIONS);
        assert!(vector.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn dimensions_are_honored() {
        assert_eq!(embed_text("hello world", 16).len(), 16);
        assert_eq!(embed_text("hello world", 1536).len(), 1536);
        assert!(embed_text("hello world", 0).is_empty());
    }

    #[test]
    fn tokenization_is_case_and_punctuation_insensitive() {
        assert_eq!(
            embed_text("Install MISE, then build!", DEFAULT_DIMENSIONS),
            embed_text("install mise then build", DEFAULT_DIMENSIONS)
        );
    }

    #[test]
    fn lexical_overlap_lands_nearer_in_cosine_space() {
        let base = embed_text("install mise then run the build", DEFAULT_DIMENSIONS);
        let near = embed_text("install mise and run tests", DEFAULT_DIMENSIONS);
        let far = embed_text("zebra quantum harpsichord", DEFAULT_DIMENSIONS);
        assert!(cosine(&base, &near) > cosine(&base, &far));
    }
}
