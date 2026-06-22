//! The [`Embedder`] seam and a dependency-free lexical v1.
//!
//! Vector similarity is used **only for memory recall, never for code retrieval**
//! (DESIGN.md §13.6). The load-bearing property is *similarity ranking over
//! embeddings*, not a specific model, so this crate freezes an [`Embedder`] seam
//! and ships a dependency-free [`LexicalEmbedder`] (a bag-of-words term-frequency
//! embedding with cosine similarity) behind it. A real learned embedder is an
//! additive impl behind the same seam (CLAUDE.md C3).
//!
//! Embeddings are sparse integer term-frequency maps so they serialize and stay
//! deterministic; cosine similarity is returned in **basis points** (0..=10000),
//! never as a float, so callers reason in integers.

use std::collections::BTreeMap;

/// A sparse term-frequency embedding (token -> count).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Embedding {
    terms: BTreeMap<String, u32>,
}

impl Embedding {
    /// Construct an embedding from a term-frequency map.
    #[must_use]
    pub fn from_terms(terms: BTreeMap<String, u32>) -> Self {
        Embedding { terms }
    }

    /// Whether the embedding has no terms.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Cosine similarity with another embedding, in basis points (0..=10000).
    ///
    /// `0` means no shared terms; `10000` means identical term distributions.
    /// Computed in `f64` internally but returned as an integer so it is stable
    /// for ranking.
    #[must_use]
    pub fn cosine_bps(&self, other: &Embedding) -> u32 {
        if self.terms.is_empty() || other.terms.is_empty() {
            return 0;
        }
        let mut dot: f64 = 0.0;
        for (term, &a) in &self.terms {
            if let Some(&b) = other.terms.get(term) {
                dot += f64::from(a) * f64::from(b);
            }
        }
        if dot == 0.0 {
            return 0;
        }
        let norm = |e: &Embedding| -> f64 {
            e.terms
                .values()
                .map(|&c| f64::from(c) * f64::from(c))
                .sum::<f64>()
                .sqrt()
        };
        let sim = dot / (norm(self) * norm(other));
        (sim.clamp(0.0, 1.0) * 10_000.0).round() as u32
    }
}

/// The embedding seam (DESIGN.md §13.6).
pub trait Embedder {
    /// Embed `text` into a similarity-comparable [`Embedding`].
    fn embed(&self, text: &str) -> Embedding;
}

/// The dependency-free v1 lexical embedder: lowercased term frequencies over
/// alphanumeric tokens of length >= 2.
#[derive(Debug, Clone, Copy, Default)]
pub struct LexicalEmbedder;

impl LexicalEmbedder {
    /// Construct the v1 lexical embedder.
    #[must_use]
    pub fn new() -> Self {
        LexicalEmbedder
    }
}

impl Embedder for LexicalEmbedder {
    fn embed(&self, text: &str) -> Embedding {
        let mut terms: BTreeMap<String, u32> = BTreeMap::new();
        let mut cur = String::new();
        let flush = |cur: &mut String, terms: &mut BTreeMap<String, u32>| {
            if cur.len() >= 2 {
                *terms.entry(std::mem::take(cur)).or_insert(0) += 1;
            } else {
                cur.clear();
            }
        };
        for c in text.chars() {
            if c.is_alphanumeric() {
                cur.extend(c.to_lowercase());
            } else {
                flush(&mut cur, &mut terms);
            }
        }
        flush(&mut cur, &mut terms);
        Embedding::from_terms(terms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_text_is_full_similarity() {
        let e = LexicalEmbedder::new();
        let a = e.embed("retry the http client with backoff");
        let b = e.embed("retry the http client with backoff");
        assert_eq!(a.cosine_bps(&b), 10_000);
    }

    #[test]
    fn disjoint_text_is_zero_similarity() {
        let e = LexicalEmbedder::new();
        let a = e.embed("alpha beta gamma");
        let b = e.embed("delta epsilon zeta");
        assert_eq!(a.cosine_bps(&b), 0);
    }

    #[test]
    fn overlap_is_partial() {
        let e = LexicalEmbedder::new();
        let a = e.embed("retry http client");
        let b = e.embed("retry http server");
        let sim = a.cosine_bps(&b);
        assert!(sim > 0 && sim < 10_000, "partial overlap: {sim}");
    }

    #[test]
    fn case_insensitive_and_min_length() {
        let e = LexicalEmbedder::new();
        let a = e.embed("HTTP Client");
        let b = e.embed("http client");
        assert_eq!(a.cosine_bps(&b), 10_000);
        // Single chars are dropped.
        assert!(e.embed("a b c").is_empty());
    }

    #[test]
    fn empty_is_zero() {
        let e = LexicalEmbedder::new();
        assert_eq!(e.embed("").cosine_bps(&e.embed("anything here")), 0);
    }
}
