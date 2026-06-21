//! Spork P7 repo map (DESIGN.md §13.6).
//!
//! The repo map is a tree-sitter-style symbol-reference graph ranked by
//! **PageRank** and bounded to a token budget, feeding the context compiler's
//! stable `repo_map` layer (rank 1, cacheable — DESIGN.md §13.2). Following call
//! chains beats similarity-matched snippets because code changes every commit
//! (DESIGN.md §13.3), and a bounded, importance-ranked map keeps the stable
//! prefix small.
//!
//! Per DESIGN.md §13.6 the parser is tree-sitter; the load-bearing invariant is
//! the *ranked, bounded symbol graph*, so this crate freezes a
//! [`SymbolExtractor`] seam and ships a complete, dependency-free v1
//! ([`KeywordSymbolExtractor`]) behind it. A full tree-sitter extractor is an
//! additive impl behind the same seam (CLAUDE.md C3) — the same narrow-but-complete
//! pattern as the P6 plaintext-vs-TLS transport.
//!
//! # Example
//!
//! ```
//! use spork_repomap::{RepoMap, KeywordSymbolExtractor, DEFAULT_REPO_MAP_TOKENS};
//!
//! let files = vec![
//!     ("src/core.rs".to_string(), "pub fn core_fn() {}".to_string()),
//!     ("src/a.rs".to_string(), "fn a() { core_fn(); core_fn(); }".to_string()),
//! ];
//! let map = RepoMap::build(&files, &KeywordSymbolExtractor::new());
//! // The widely-referenced symbol ranks first.
//! assert_eq!(map.ranked_symbols()[0].symbol.name, "core_fn");
//! let rendered = map.render(DEFAULT_REPO_MAP_TOKENS);
//! assert!(rendered.contains("core_fn"));
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod extract;
mod map;

pub use extract::{KeywordSymbolExtractor, Symbol, SymbolExtractor, SymbolKind};
pub use map::{RankedSymbol, RepoMap, DEFAULT_REPO_MAP_TOKENS};
