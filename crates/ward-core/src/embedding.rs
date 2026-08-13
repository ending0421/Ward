//! L3 embedding interface (spec §3-M1, fourth fingerprint layer).
//!
//! Honest scope: the *interface* and the deterministic degradation path are
//! the deliverables here. The default local provider is a **feature-hashing
//! bag-of-identifier-tokens embedder** — a deterministic lexical
//! representation, NOT a learned semantic model. It catches token-bag
//! similarity (renamed functions with shared domain vocabulary) but is not
//! semantic equivalence; a learned provider (fastembed/onnx, per spec §3.0
//! language eval gates) plugs into the same trait. When no provider is
//! configured, Spot already degrades to L0–L2 + BM25 (F8).

use crate::search::tokenize;

/// A pluggable text embedder.
pub trait EmbeddingProvider {
    /// Embed text, or `None` when unavailable (F8 degradation).
    fn embed(&self, text: &str) -> Option<Vec<f32>>;
}

/// 128-dim feature-hashing embedder over identifier tokens.
#[derive(Debug, Clone, Default)]
pub struct HashingEmbedder {
    dim: usize,
}

impl HashingEmbedder {
    pub fn new(dim: usize) -> Self {
        Self { dim: dim.max(1) }
    }

    pub fn dim(&self) -> usize {
        self.dim
    }
}

fn hash_token(token: &str) -> u64 {
    let mut h = blake3::Hasher::new();
    h.update(token.as_bytes());
    let out = h.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&out.as_bytes()[..8]);
    u64::from_le_bytes(bytes)
}

impl EmbeddingProvider for HashingEmbedder {
    fn embed(&self, text: &str) -> Option<Vec<f32>> {
        let tokens = tokenize(text);
        if tokens.is_empty() {
            return None;
        }
        let mut v = vec![0f32; self.dim];
        for t in &tokens {
            let h = hash_token(t);
            let idx = (h as usize) % self.dim;
            let sign = if (h >> 63) & 1 == 1 { -1.0 } else { 1.0 };
            v[idx] += sign;
        }
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        Some(v)
    }
}

/// Cosine similarity in [-1, 1]; empty vectors are unrelated.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_text_is_unit_similarity() {
        let e = HashingEmbedder::new(128);
        let a = e.embed("debounce leading trailing").unwrap();
        let b = e.embed("debounce leading trailing").unwrap();
        assert!((cosine(&a, &b) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn shared_vocabulary_scores_higher_than_disjoint() {
        let e = HashingEmbedder::new(128);
        let base = e.embed("debounce timer callback").unwrap();
        let similar = e.embed("throttle timer callback").unwrap();
        let disjoint = e.embed("quicksort pivot partition").unwrap();
        assert!(
            cosine(&base, &similar) > cosine(&base, &disjoint),
            "shared tokens must dominate"
        );
    }

    #[test]
    fn empty_text_yields_none() {
        let e = HashingEmbedder::new(64);
        assert!(e.embed("").is_none());
        assert!(e.embed("___").is_none());
    }

    #[test]
    fn cosine_of_mismatched_lengths_is_zero() {
        assert_eq!(cosine(&[1.0], &[1.0, 1.0]), 0.0);
        assert_eq!(cosine(&[], &[]), 0.0);
    }

    #[test]
    fn embeddings_are_unit_normalized() {
        let e = HashingEmbedder::new(128);
        let v = e.embed("a b c d e").unwrap();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4);
    }
}
