//! The PageRank-ranked, token-bounded [`RepoMap`] (DESIGN.md §13.6).
//!
//! The repo map is built from a symbol-reference graph: files are nodes, and a
//! reference from file *A* to a symbol defined in file *B* is an edge *A→B*
//! weighted by mention count. **PageRank** over that graph ranks files by
//! importance; each symbol inherits its defining file's rank plus a bonus for how
//! widely it is referenced. The rendered map lists the top symbols within a token
//! budget (Aider-style ~1k tokens by default), so the compiler's stable `repo_map`
//! layer (rank 1) stays small (DESIGN.md §13.2, §13.6).
//!
//! PageRank runs in `f64` but with **sorted node iteration and a fixed iteration
//! count**, and final ranks are rounded to integer milli-units before sorting, so
//! the rendered string is deterministic across machines — which matters because
//! it feeds the `prefix_hash`.

use std::collections::{BTreeMap, BTreeSet};

use crate::extract::{Symbol, SymbolExtractor};

/// The default repo-map token budget (Aider-style ~1k).
pub const DEFAULT_REPO_MAP_TOKENS: u64 = 1024;

/// PageRank damping factor.
const DAMPING: f64 = 0.85;
/// PageRank iteration count (fixed for determinism).
const ITERATIONS: usize = 30;

/// A symbol with its computed importance score (milli-units; higher = more
/// important).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankedSymbol {
    /// The symbol.
    pub symbol: Symbol,
    /// Its importance score in milli-units.
    pub score: u64,
}

/// A PageRank-ranked, token-bounded repo map (DESIGN.md §13.6).
#[derive(Debug, Clone, Default)]
pub struct RepoMap {
    ranked: Vec<RankedSymbol>,
    file_rank_milli: BTreeMap<String, u64>,
}

impl RepoMap {
    /// Build a repo map from `(path, source)` files using a [`SymbolExtractor`].
    #[must_use]
    pub fn build(files: &[(String, String)], extractor: &dyn SymbolExtractor) -> Self {
        // 1. Collect definitions and the name -> defining-files index.
        let mut symbols: Vec<Symbol> = Vec::new();
        let mut def_files: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut all_files: BTreeSet<String> = BTreeSet::new();
        for (path, source) in files {
            all_files.insert(path.clone());
            for sym in extractor.definitions(path, source) {
                def_files
                    .entry(sym.name.clone())
                    .or_default()
                    .insert(path.clone());
                symbols.push(sym);
            }
        }
        symbols.sort();
        symbols.dedup();

        // 2. Build the file reference graph + per-symbol external mention counts.
        let mut edges: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
        let mut symbol_mentions: BTreeMap<(String, String), u64> = BTreeMap::new();
        for (path, source) in files {
            for name in extractor.references(path, source) {
                if let Some(targets) = def_files.get(&name) {
                    for target in targets {
                        if target != path {
                            *edges
                                .entry(path.clone())
                                .or_default()
                                .entry(target.clone())
                                .or_insert(0) += 1;
                            *symbol_mentions
                                .entry((name.clone(), target.clone()))
                                .or_insert(0) += 1;
                        }
                    }
                }
            }
        }

        // 3. PageRank over files.
        let file_rank_milli = pagerank(&all_files, &edges);

        // 4. Score each symbol: defining-file rank + a per-mention bonus.
        let mut ranked: Vec<RankedSymbol> = symbols
            .into_iter()
            .map(|sym| {
                let file_rank = file_rank_milli.get(&sym.file).copied().unwrap_or(0);
                let mentions = symbol_mentions
                    .get(&(sym.name.clone(), sym.file.clone()))
                    .copied()
                    .unwrap_or(0);
                // File rank dominates; mentions break ties and lift widely-used
                // symbols within a file.
                let score = file_rank
                    .saturating_mul(1000)
                    .saturating_add(mentions * 100);
                RankedSymbol { symbol: sym, score }
            })
            .collect();
        // Sort by score desc, then file asc, then name asc — fully deterministic.
        ranked.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.symbol.file.cmp(&b.symbol.file))
                .then_with(|| a.symbol.name.cmp(&b.symbol.name))
        });

        RepoMap {
            ranked,
            file_rank_milli,
        }
    }

    /// The ranked symbols, most-important first.
    #[must_use]
    pub fn ranked_symbols(&self) -> &[RankedSymbol] {
        &self.ranked
    }

    /// A file's PageRank score in milli-units.
    #[must_use]
    pub fn file_rank(&self, file: &str) -> u64 {
        self.file_rank_milli.get(file).copied().unwrap_or(0)
    }

    /// Render the map within a token budget, grouping symbols under their file in
    /// importance order (DESIGN.md §13.6). The output is deterministic.
    #[must_use]
    pub fn render(&self, token_budget: u64) -> String {
        // Group the top symbols by file, preserving the global importance order
        // of files (a file appears at the position of its best symbol).
        let mut file_order: Vec<String> = Vec::new();
        let mut by_file: BTreeMap<String, Vec<&Symbol>> = BTreeMap::new();
        for rs in &self.ranked {
            if !by_file.contains_key(&rs.symbol.file) {
                file_order.push(rs.symbol.file.clone());
            }
            by_file
                .entry(rs.symbol.file.clone())
                .or_default()
                .push(&rs.symbol);
        }

        let mut out = String::new();
        let mut used: u64 = 0;
        for file in &file_order {
            let header = format!("{file}:\n");
            let header_cost = estimate_tokens(&header);
            if used + header_cost > token_budget {
                break;
            }
            let mut wrote_header = false;
            for sym in &by_file[file] {
                let line = format!("  {} {}\n", sym.kind.label(), sym.name);
                let cost = estimate_tokens(&line);
                if used + cost + if wrote_header { 0 } else { header_cost } > token_budget {
                    break;
                }
                if !wrote_header {
                    out.push_str(&header);
                    used += header_cost;
                    wrote_header = true;
                }
                out.push_str(&line);
                used += cost;
            }
        }
        out
    }
}

/// A deterministic ~4-bytes-per-token estimate (mirrors `spork_context`).
fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(4)
}

/// Power-iteration PageRank over the file graph, returning milli-unit ranks.
///
/// Deterministic: nodes are iterated in sorted order and the iteration count is
/// fixed; ranks are rounded to integer milli-units.
fn pagerank(
    nodes: &BTreeSet<String>,
    edges: &BTreeMap<String, BTreeMap<String, u64>>,
) -> BTreeMap<String, u64> {
    let files: Vec<&String> = nodes.iter().collect();
    let n = files.len();
    if n == 0 {
        return BTreeMap::new();
    }
    let index: BTreeMap<&String, usize> = files.iter().enumerate().map(|(i, f)| (*f, i)).collect();

    // Out-edges as (target_index, weight) and per-node out-weight sums.
    let mut out: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    let mut out_sum: Vec<f64> = vec![0.0; n];
    for (from, targets) in edges {
        if let Some(&fi) = index.get(from) {
            for (to, &w) in targets {
                if let Some(&ti) = index.get(to) {
                    out[fi].push((ti, w as f64));
                    out_sum[fi] += w as f64;
                }
            }
        }
    }

    let base = (1.0 - DAMPING) / n as f64;
    let mut rank = vec![1.0 / n as f64; n];
    for _ in 0..ITERATIONS {
        let mut next = vec![base; n];
        // Dangling mass (nodes with no out-edges) redistributes uniformly.
        let mut dangling = 0.0;
        for i in 0..n {
            if out_sum[i] == 0.0 {
                dangling += rank[i];
            }
        }
        let dangling_share = DAMPING * dangling / n as f64;
        for slot in next.iter_mut() {
            *slot += dangling_share;
        }
        for i in 0..n {
            if out_sum[i] > 0.0 {
                for &(ti, w) in &out[i] {
                    next[ti] += DAMPING * rank[i] * (w / out_sum[i]);
                }
            }
        }
        rank = next;
    }

    files
        .iter()
        .enumerate()
        .map(|(i, f)| ((*f).clone(), (rank[i] * 1_000_000.0).round() as u64))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::KeywordSymbolExtractor;

    fn files() -> Vec<(String, String)> {
        vec![
            // core.rs defines `core_fn`, referenced by everyone -> most important.
            ("src/core.rs".into(), "pub fn core_fn() {}".into()),
            (
                "src/a.rs".into(),
                "fn a_fn() { core_fn(); core_fn(); }".into(),
            ),
            ("src/b.rs".into(), "fn b_fn() { core_fn(); a_fn(); }".into()),
        ]
    }

    #[test]
    fn central_file_ranks_highest() {
        let map = RepoMap::build(&files(), &KeywordSymbolExtractor::new());
        // core.rs is referenced the most, so it outranks the leaves.
        assert!(map.file_rank("src/core.rs") > map.file_rank("src/b.rs"));
    }

    #[test]
    fn central_symbol_is_ranked_first() {
        let map = RepoMap::build(&files(), &KeywordSymbolExtractor::new());
        let top = &map.ranked_symbols()[0];
        assert_eq!(top.symbol.name, "core_fn");
    }

    #[test]
    fn render_is_bounded_and_deterministic() {
        let map = RepoMap::build(&files(), &KeywordSymbolExtractor::new());
        let full = map.render(DEFAULT_REPO_MAP_TOKENS);
        assert!(full.contains("core_fn"));
        // Deterministic across calls.
        assert_eq!(full, map.render(DEFAULT_REPO_MAP_TOKENS));
        // A tiny budget truncates without panicking and stays within budget.
        let tiny = map.render(8);
        assert!(estimate_tokens(&tiny) <= 8);
    }

    #[test]
    fn render_groups_symbols_under_files() {
        let map = RepoMap::build(
            &[(
                "src/core.rs".into(),
                "fn core_fn() {} fn helper() {}".into(),
            )],
            &KeywordSymbolExtractor::new(),
        );
        let out = map.render(DEFAULT_REPO_MAP_TOKENS);
        assert!(out.contains("src/core.rs:"));
        assert!(out.contains("fn core_fn"));
        assert!(out.contains("fn helper"));
    }

    #[test]
    fn empty_repo_renders_empty() {
        let map = RepoMap::build(&[], &KeywordSymbolExtractor::new());
        assert_eq!(map.render(DEFAULT_REPO_MAP_TOKENS), "");
        assert!(map.ranked_symbols().is_empty());
    }

    #[test]
    fn build_is_deterministic_across_runs() {
        let a = RepoMap::build(&files(), &KeywordSymbolExtractor::new());
        let b = RepoMap::build(&files(), &KeywordSymbolExtractor::new());
        assert_eq!(
            a.render(DEFAULT_REPO_MAP_TOKENS),
            b.render(DEFAULT_REPO_MAP_TOKENS)
        );
    }
}
